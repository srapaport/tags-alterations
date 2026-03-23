use anyhow::Result;
use chrono::prelude::*;
use dotenv::dotenv;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::File;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use swh_graph::labels::EdgeLabel;
use swh_graph::mph::DynMphf;
use swh_graph::{NodeType, graph::*};
use tags_alterations::snapshots_extraction;

#[derive(Debug, Clone, Default)]
struct MonthlyNetCounters {
    initial_observed: usize,
    count_delta: isize,
}

static COUNTER_INVALID_UTF8_BRANCH_NAME: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGIN_ANALYZED: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGIN_URL_NOT_UTF8: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGIN_WITH_TAG: AtomicUsize = AtomicUsize::new(0);
static COUNTER_INITIAL_TAGS_OBSERVED: AtomicUsize = AtomicUsize::new(0);
static COUNTER_OBSERVABLE_DELTA: AtomicUsize = AtomicUsize::new(0);
static COUNTER_SNAPSHOTS_PROCESSED: AtomicUsize = AtomicUsize::new(0);
static COUNTER_MONTHLY_SNAPSHOTS_SELECTED: AtomicUsize = AtomicUsize::new(0);

// Faster tag counting for a snapshot: use label_name_id dedup instead of full tag strings.
fn count_snapshot_tags<G: SwhFullGraph>(
    snapshot: usize,
    graph: &G,
    is_tag_cache: &mut HashMap<swh_graph::labels::LabelNameId, bool>,
) -> usize {
    let mut tags = HashSet::new();
    for (succ, labels) in graph.labeled_successors(snapshot) {
        let succ_type = graph.properties().node_type(succ);
        if ![NodeType::Release, NodeType::Revision].contains(&succ_type) {
            continue;
        }
        for label in labels {
            if let EdgeLabel::Branch(branch) = label {
                let label_id = branch.label_name_id();
                let is_tag = *is_tag_cache.entry(label_id).or_insert_with(|| {
                    let name = graph.properties().label_name(label_id);
                    name.windows(6).any(|w| w == b"/tags/")
                });
                if is_tag {
                    tags.insert(label_id);
                }
            }
        }
    }
    tags.len()
}

// Main function that tracks net tag changes between snapshots
fn main() -> Result<()> {
    dotenv()?;
    let graph_basename = env::var("GRAPH_BASENAME").expect("GRAPH_BASENAME not set");
    let suffix = env::var("DATASET_SUFFIX").unwrap_or_else(|_| "full_2025-10_v2".to_string());
    
    println!("Load graph...");
    let graph = SwhBidirectionalGraph::new(graph_basename)?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;

    println!("Extract snapshots from ORC files...");
    let origin_snapshots = snapshots_extraction(&suffix)?;
    let total_origins = origin_snapshots.len();
    println!("Extracted {} origins with snapshots", total_origins);

    println!("Start tracking observable tags with monthly snapshot collapsing...");
    let pb = Arc::new(ProgressBar::new(total_origins as u64));
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{msg} [{bar:40.cyan/blue}] {pos}/{len} origins {percent}% ({per_sec}, {eta})")?
            .progress_chars("#>-"),
    );
    pb.set_message("Processing origins");

    let monthly_stats: HashMap<(i32, u32), MonthlyNetCounters> = origin_snapshots
        .into_par_iter()
        .filter_map(|(origin_url, snapshot_infos)| {
            let pb = Arc::clone(&pb);
            let mut local_monthly: HashMap<(i32, u32), MonthlyNetCounters> = HashMap::new();
            
            // Get origin node from URL
            let origin_swhid = swh_graph::SWHID::from_origin_url(&origin_url);
            let _origin = match graph.properties().node_id(origin_swhid) {
                Ok(node) => node,
                Err(_) => {
                    COUNTER_ORIGIN_URL_NOT_UTF8.fetch_add(1, Ordering::Relaxed);
                    pb.inc(1);
                    return None;
                }
            };
            
            COUNTER_ORIGIN_ANALYZED.fetch_add(1, Ordering::Relaxed);
            
            // Keep only the latest snapshot per month for this origin.
            let mut latest_by_month: HashMap<(i32, u32), (usize, u64)> = HashMap::new();
            for info in snapshot_infos {
                let Some(snapshot_swhid_hash) = info.snapshot else {
                    continue;
                };
                let snapshot_swhid = format!("swh:1:snp:{}", snapshot_swhid_hash);
                if let Ok(snapshot_node) = graph.properties().node_id(snapshot_swhid.as_str()) {
                    let timestamp = info.date_seconds as u64;
                    let dt = Utc.timestamp_opt(timestamp as i64, 0).unwrap();
                    let year_month = (dt.year(), dt.month());
                    let entry = latest_by_month
                        .entry(year_month)
                        .or_insert((snapshot_node, timestamp));
                    if timestamp > entry.1 {
                        *entry = (snapshot_node, timestamp);
                    }
                }
            }
            
            if latest_by_month.is_empty() {
                pb.inc(1);
                return None;
            }
            
            let mut months: Vec<_> = latest_by_month.into_iter().collect();
            months.sort_by_key(|(ym, _)| *ym);

            let mut prev_count: Option<usize> = None;
            let mut is_tag_cache = HashMap::new();

            for (year_month, (snapshot, _timestamp)) in months {
                COUNTER_MONTHLY_SNAPSHOTS_SELECTED.fetch_add(1, Ordering::Relaxed);
                COUNTER_SNAPSHOTS_PROCESSED.fetch_add(1, Ordering::Relaxed);
                let current_count = count_snapshot_tags(snapshot, &graph, &mut is_tag_cache);

                if let Some(prev) = prev_count {
                    let delta = current_count as isize - prev as isize;
                    if delta != 0 {
                        let counter = local_monthly
                            .entry(year_month)
                            .or_insert_with(MonthlyNetCounters::default);
                        counter.count_delta += delta;
                        COUNTER_OBSERVABLE_DELTA.fetch_add(delta.unsigned_abs(), Ordering::Relaxed);
                    }
                } else if current_count > 0 {
                    let counter = local_monthly
                        .entry(year_month)
                        .or_insert_with(MonthlyNetCounters::default);
                    counter.initial_observed += current_count;
                    COUNTER_ORIGIN_WITH_TAG.fetch_add(1, Ordering::Relaxed);
                    COUNTER_INITIAL_TAGS_OBSERVED.fetch_add(current_count, Ordering::Relaxed);
                }

                prev_count = Some(current_count);
            }
            
            pb.inc(1);
            Some(local_monthly)
        })
        .reduce(
            || HashMap::new(),
            |mut monthly_a, monthly_b| {
                for ((year, month), counters) in monthly_b {
                    let entry = monthly_a
                        .entry((year, month))
                        .or_insert_with(MonthlyNetCounters::default);
                    entry.initial_observed += counters.initial_observed;
                    entry.count_delta += counters.count_delta;
                }
                monthly_a
            },
        );

    pb.finish_with_message("Processing complete");

    let output = format_net_counters(&monthly_stats);
    println!("{}", output);

    let mut log_file = File::create("data/counts/tags_count_net_v2.log")?;
    log_file.write_all(output.as_bytes())?;

    Ok(())
}

fn format_net_counters(
    monthly_stats: &HashMap<(i32, u32), MonthlyNetCounters>,
) -> String {
    let total_snapshots = COUNTER_SNAPSHOTS_PROCESSED.load(Ordering::Relaxed);
    let total_monthly_snapshots = COUNTER_MONTHLY_SNAPSHOTS_SELECTED.load(Ordering::Relaxed);
    let total_initial = COUNTER_INITIAL_TAGS_OBSERVED.load(Ordering::Relaxed);
    let total_delta_magnitude = COUNTER_OBSERVABLE_DELTA.load(Ordering::Relaxed);
    let total_delta: isize = monthly_stats.values().map(|c| c.count_delta).sum();
    
    let mut output = format!(
        "\n\nTag Counting Results (net changes per month)\n\
        ============================================\n\
        Analyzed {} origins | Skipped {} origins (no UTF8 URL)\n\
        Origins with tags: {}\n\
        Monthly snapshots selected (latest/origin/month): {}\n\
        Total snapshot counts executed: {}\n\n\
                Total observed stock components:\n\
                    - Initial observed tags (first snapshots): {}\n\
          - Net delta after first month per origin: {}\n\
          - Delta magnitude traversed: {}\n\
                    - Final observable tags estimate: {}\n\
        Invalid branch names (not UTF8): {}\n\n",
        COUNTER_ORIGIN_ANALYZED.load(Ordering::Relaxed),
        COUNTER_ORIGIN_URL_NOT_UTF8.load(Ordering::Relaxed),
        COUNTER_ORIGIN_WITH_TAG.load(Ordering::Relaxed),
        total_monthly_snapshots,
        total_snapshots,
        total_initial,
        total_delta,
        total_delta_magnitude,
        (total_initial as isize + total_delta),
        COUNTER_INVALID_UTF8_BRANCH_NAME.load(Ordering::Relaxed),
    );

    if !monthly_stats.is_empty() {
        output.push_str("\nMonthly Evolution (Observable Stock):\n");
        output.push_str("=================================\n");
        
        let mut sorted_months: Vec<_> = monthly_stats.iter().collect();
        sorted_months.sort_by_key(|(key, _)| *key);
        
        output.push_str(&format!(
            "{:<12} {:>12} {:>12} {:>15}\n",
            "Month", "Initial", "Delta", "Observable"
        ));
        output.push_str(&format!("{:-<58}\n", ""));
        
        let mut cumulative_observable = 0isize;
        for (&(year, month), counters) in sorted_months {
            cumulative_observable += counters.initial_observed as isize + counters.count_delta;
            output.push_str(&format!(
                "{:04}-{:02}      {:>12} {:>12} {:>15}\n",
                year, month,
                counters.initial_observed,
                counters.count_delta,
                cumulative_observable,
            ));
        }
        output.push_str(&format!(
            "\nFinal cumulative observable tags: {}\n",
            cumulative_observable
        ));
    }

    output
}
