use anyhow::{Result, anyhow};
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
use tags_alterations::lib_tmp::snapshots_extraction;

static COUNTER_ORIGIN_ANALYZED: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGIN_WITH_SNAPSHOTS: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGIN_NO_VALID_SNAPSHOT: AtomicUsize = AtomicUsize::new(0);
static COUNTER_SNAPSHOTS_PROCESSED: AtomicUsize = AtomicUsize::new(0);
static COUNTER_SNAPSHOT_SWHID_NOT_FOUND: AtomicUsize = AtomicUsize::new(0);
static COUNTER_COMMITS_FOUND: AtomicUsize = AtomicUsize::new(0);
static COUNTER_COMMITS_WITH_TIMESTAMP: AtomicUsize = AtomicUsize::new(0);
static COUNTER_COMMITS_NO_TIMESTAMP: AtomicUsize = AtomicUsize::new(0);

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

    println!("Start counting commits from last snapshots...");
    let pb = Arc::new(ProgressBar::new(total_origins as u64));
    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "{msg} [{bar:40.cyan/blue}] {pos}/{len} origins {percent}% ({per_sec}, {eta})",
            )?
            .progress_chars("#>-"),
    );
    pb.set_message("Processing origins");

    let monthly_commits: HashMap<(i32, u32), usize> = origin_snapshots
        .into_par_iter()
        .filter_map(|(origin_url, snapshots)| {
            COUNTER_ORIGIN_ANALYZED.fetch_add(1, Ordering::Relaxed);

            let mut local_monthly: HashMap<(i32, u32), usize> = HashMap::new();

            // Get the last snapshot (snapshots are already sorted)
            let Some(last_snapshot_info) = snapshots.last() else {
                COUNTER_ORIGIN_NO_VALID_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
                pb.inc(1);
                return None;
            };

            COUNTER_ORIGIN_WITH_SNAPSHOTS.fetch_add(1, Ordering::Relaxed);
            COUNTER_SNAPSHOTS_PROCESSED.fetch_add(1, Ordering::Relaxed);

            // Get the snapshot node from the SWHID
            let Some(snapshot_swhid_hash) = &last_snapshot_info.snapshot else {
                pb.inc(1);
                return None;
            };

            let snapshot_swhid = format!("swh:1:snp:{}", snapshot_swhid_hash);
            let snapshot_node = match graph.properties().node_id(snapshot_swhid.as_str()) {
                Ok(node) => node,
                Err(_) => {
                    COUNTER_SNAPSHOT_SWHID_NOT_FOUND.fetch_add(1, Ordering::Relaxed);
                    pb.inc(1);
                    return None;
                }
            };

            // Traverse the snapshot to find all reachable commits
            let mut visited_commits = HashSet::new();
            let mut to_visit = Vec::new();
            to_visit.push(snapshot_node);

            // BFS to collect all commits
            while let Some(node) = to_visit.pop() {
                if !visited_commits.insert(node) {
                    continue; // Already visited
                }

                let node_type = graph.properties().node_type(node);
                
                // If current node is a commit, count it
                if node_type == NodeType::Revision {
                    COUNTER_COMMITS_FOUND.fetch_add(1, Ordering::Relaxed);
                    if let Some(timestamp) = graph.properties().committer_timestamp(node) {
                        COUNTER_COMMITS_WITH_TIMESTAMP.fetch_add(1, Ordering::Relaxed);
                        let dt = Utc.timestamp_opt(timestamp, 0).unwrap();
                        let year_month = (dt.year(), dt.month());
                        let counter = local_monthly.entry(year_month).or_insert(0);
                        *counter += 1;
                    } else {
                        COUNTER_COMMITS_NO_TIMESTAMP.fetch_add(1, Ordering::Relaxed);
                    }
                }

                // Add successors to visit queue (branches, releases, and parent commits)
                for successor in graph.successors(node) {
                    let succ_type = graph.properties().node_type(successor);
                    if matches!(succ_type, NodeType::Release | NodeType::Revision) {
                        to_visit.push(successor);
                    }
                }
            }

            pb.inc(1);
            Some(local_monthly)
        })
        .reduce(
            || HashMap::new(),
            |mut monthly_a, monthly_b| {
                for ((year, month), count) in monthly_b {
                    let entry = monthly_a.entry((year, month)).or_insert(0);
                    *entry += count;
                }
                monthly_a
            },
        );

    pb.finish_with_message("Processing complete");

    let output = format_results(&monthly_commits);
    println!("{}", output);

    let mut log_file = File::create(format!("data/commits_per_month_{}.log", suffix))?;
    log_file.write_all(output.as_bytes())?;

    Ok(())
}

fn format_results(monthly_commits: &HashMap<(i32, u32), usize>) -> String {
    let total_origins = COUNTER_ORIGIN_ANALYZED.load(Ordering::Relaxed);
    let origins_with_snapshots = COUNTER_ORIGIN_WITH_SNAPSHOTS.load(Ordering::Relaxed);
    let origins_no_snapshots = COUNTER_ORIGIN_NO_VALID_SNAPSHOT.load(Ordering::Relaxed);
    let total_snapshots = COUNTER_SNAPSHOTS_PROCESSED.load(Ordering::Relaxed);
    let snapshot_swhid_not_found = COUNTER_SNAPSHOT_SWHID_NOT_FOUND.load(Ordering::Relaxed);
    let commits_found = COUNTER_COMMITS_FOUND.load(Ordering::Relaxed);
    let commits_with_ts = COUNTER_COMMITS_WITH_TIMESTAMP.load(Ordering::Relaxed);
    let commits_no_ts = COUNTER_COMMITS_NO_TIMESTAMP.load(Ordering::Relaxed);

    let mut output = format!(
        "\n\nCommits Per Month Counting Results\n\
        ===================================\n\
        Analyzed {} origins\n\
        Origins with valid snapshots: {}\n\
        Origins without valid snapshots: {}\n\
        Last snapshots processed: {}\n\
        Snapshot SWHIDs not found in graph: {}\n\
        \n\
        Commits statistics:\n\
          - Total commits found: {}\n\
          - Commits with timestamp: {}\n\
          - Commits without timestamp: {}\n\n",
        total_origins,
        origins_with_snapshots,
        origins_no_snapshots,
        total_snapshots,
        snapshot_swhid_not_found,
        commits_found,
        commits_with_ts,
        commits_no_ts,
    );

    if !monthly_commits.is_empty() {
        output.push_str("\nMonthly Evolution (Commits):\n");
        output.push_str("============================\n");

        let mut sorted_months: Vec<_> = monthly_commits.iter().collect();
        sorted_months.sort_by_key(|(key, _)| *key);

        output.push_str(&format!(
            "{:<12} {:>15} {:>15}\n",
            "Month", "Commits", "Cumulative"
        ));
        output.push_str(&format!("{:-<45}\n", ""));

        let mut cumulative = 0usize;
        for (&(year, month), count) in sorted_months {
            cumulative += count;
            output.push_str(&format!(
                "{:04}-{:02}      {:>15} {:>15}\n",
                year, month, count, cumulative
            ));
        }
        output.push_str(&format!("\nTotal commits counted: {}\n", cumulative));
    }

    output
}
