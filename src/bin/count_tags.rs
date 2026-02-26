use anyhow::{Result, anyhow};
use chrono::prelude::*;
use dotenv::dotenv;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use swh_graph::labels::EdgeLabel;
use swh_graph::mph::DynMphf;
use swh_graph::{NodeType, graph::*};

#[derive(Debug, Clone, Default)]
struct MonthlyCounters {
    lightweight: usize,
    annotated: usize,
    unknown: usize,
}

static COUNTER_INVALID_UTF8_BRANCH_NAME: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_LIGHTWEIGHT: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_ANNOTATED: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_UNKNOWN: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_NO_TIMESTAMP: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGIN_ANALYZED: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGIN_URL_NOT_UTF8: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGIN_WITH_TAG: AtomicUsize = AtomicUsize::new(0);

fn main() -> Result<()> {
    dotenv()?;
    let graph_basename = env::var("GRAPH_BASENAME").expect("GRAPH_BASENAME not set");
    let amount_origins: u64 = env::var("ORIGINS")
        .expect("ORIGINS not set")
        .parse()
        .map_err(|e| anyhow!("Invalid ORIGINS value: {}", e))?;
    println!("Load graph...");
    let graph = SwhBidirectionalGraph::new(graph_basename)?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;

    println!("Start Counting...");
    let pb = Arc::new(ProgressBar::new(amount_origins));
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{msg} [{bar:40.cyan/blue}] {pos}/{len} origins {percent}% ({per_sec}, {eta})")?
            .progress_chars("#>-"),
    );
    pb.set_message("Collecting tags");

    let (tags, monthly_stats): (HashMap<_, _>, HashMap<_, _>) = (0..graph.num_nodes())
        .into_par_iter()
        .filter(|node| graph.properties().node_type(*node) == NodeType::Origin)
        .map(|origin| {
            let pb = Arc::clone(&pb);
            let mut origin_url_cache: Option<String> = None;
            let mut local_tags: HashMap<String, &str> = HashMap::new();
            let mut local_monthly: HashMap<(i32, u32), MonthlyCounters> = HashMap::new();

            graph
                .successors(origin)
                .filter(|snapshot| graph.properties().node_type(*snapshot) == NodeType::Snapshot)
                .for_each(|snapshot| {
                    graph
                        .labeled_successors(snapshot)
                        .filter(|(succ, _)| {
                            [NodeType::Release, NodeType::Revision]
                                .contains(&graph.properties().node_type(*succ))
                        })
                        .for_each(|(rel, labels)| {
                            if origin_url_cache.is_none() {
                                let Some(origin_bytes) = graph.properties().message(origin) else {
                                    return;
                                };
                                let Ok(url) = String::from_utf8(origin_bytes) else {
                                    return;
                                };
                                origin_url_cache = Some(url);
                            }

                            for label in labels {
                                if let EdgeLabel::Branch(branch) = label {
                                    let Ok(branch_name) = String::from_utf8(
                                        graph.properties().label_name(branch.label_name_id()),
                                    ) else {
                                        COUNTER_INVALID_UTF8_BRANCH_NAME
                                            .fetch_add(1, Ordering::Relaxed);
                                        continue;
                                    };
                                    if branch_name.contains("/tags/") {
                                        let (tag_type, timestamp_opt) = match graph.properties().node_type(rel) {
                                            NodeType::Release => (
                                                "annotated",
                                                graph.properties().author_timestamp(rel),
                                            ),
                                            NodeType::Revision => (
                                                "lightweight",
                                                graph.properties().committer_timestamp(rel),
                                            ),
                                            _ => ("unknown", None),
                                        };
                                        
                                        if local_tags.insert(branch_name.clone(), tag_type) == None {
                                            match tag_type {
                                                "annotated" => COUNTER_TAG_ANNOTATED
                                                    .fetch_add(1, Ordering::Relaxed),
                                                "lightweight" => COUNTER_TAG_LIGHTWEIGHT
                                                    .fetch_add(1, Ordering::Relaxed),
                                                _ => COUNTER_TAG_UNKNOWN
                                                    .fetch_add(1, Ordering::Relaxed),
                                            };
                                            
                                            // Track monthly statistics
                                            if let Some(ts) = timestamp_opt {
                                                let dt = Utc.timestamp_opt(ts, 0).unwrap();
                                                let year_month = (dt.year(), dt.month());
                                                let counter = local_monthly
                                                    .entry(year_month)
                                                    .or_insert_with(MonthlyCounters::default);
                                                match tag_type {
                                                    "annotated" => counter.annotated += 1,
                                                    "lightweight" => counter.lightweight += 1,
                                                    _ => counter.unknown += 1,
                                                }
                                            } else {
                                                COUNTER_TAG_NO_TIMESTAMP
                                                    .fetch_add(1, Ordering::Relaxed);
                                            }
                                        }
                                    }
                                }
                            }
                        });
                });
            pb.inc(1);

            // origin_url_cache.into_iter().flat_map(move |url| {
            //     local_tags.into_iter().map(move |(tag_name, tag_type)| {
            //         (url.clone(), (tag_name, tag_type))
            //     })
            // })
            (origin_url_cache, local_tags, local_monthly)
        })
        .fold(
            || (HashMap::new(), HashMap::new()),
            |(mut tags_acc, mut monthly_acc), (url, tag, monthly)| {
                if let Some(origin) = url {
                    COUNTER_ORIGIN_ANALYZED.fetch_add(1, Ordering::Relaxed);
                    if !tag.is_empty() {
                        COUNTER_ORIGIN_WITH_TAG.fetch_add(1, Ordering::Relaxed);
                    }
                    tags_acc.entry(origin).or_insert_with(HashMap::new).extend(tag);
                } else {
                    COUNTER_ORIGIN_URL_NOT_UTF8.fetch_add(1, Ordering::Relaxed);
                }
                for ((year, month), counters) in monthly {
                    let entry = monthly_acc
                        .entry((year, month))
                        .or_insert_with(MonthlyCounters::default);
                    entry.lightweight += counters.lightweight;
                    entry.annotated += counters.annotated;
                    entry.unknown += counters.unknown;
                }
                (tags_acc, monthly_acc)
            },
        )
        .reduce(
            || (HashMap::new(), HashMap::new()),
            |(mut tags_a, mut monthly_a), (mut tags_b, monthly_b)| {
                if tags_a.len() < tags_b.len() {
                    std::mem::swap(&mut tags_a, &mut tags_b);
                }
                for (k, v) in tags_b {
                    tags_a.entry(k).or_insert_with(HashMap::new).extend(v);
                }
                for ((year, month), counters) in monthly_b {
                    let entry = monthly_a
                        .entry((year, month))
                        .or_insert_with(MonthlyCounters::default);
                    entry.lightweight += counters.lightweight;
                    entry.annotated += counters.annotated;
                    entry.unknown += counters.unknown;
                }
                (tags_a, monthly_a)
            },
        );

    pb.finish_with_message("Tags collected");

    let total_tags: usize = tags.values().map(|v: &HashMap<String, &str>| v.len()).sum();
    let output = format_counters(total_tags, &monthly_stats);
    println!("{}", output);

    let mut log_file = File::create("data/tags_count.log")?;
    log_file.write_all(output.as_bytes())?;

    Ok(())
}

fn format_counters(
    total_tags: usize,
    monthly_stats: &HashMap<(i32, u32), MonthlyCounters>,
) -> String {
    let mut output = format!(
        "\n\nAnalized {} origins | Skipped {} origins\n
        Collected {} origins with {} total tags\n
          - Lightweight tags: {}\n
          - Annotated tags: {}\n
          - Unknown tags: {}\n
          - Tags without timestamp: {}\n
          - Unknown branch name (not utf8): {}\n\n",
        COUNTER_ORIGIN_ANALYZED.load(Ordering::Relaxed),
        COUNTER_ORIGIN_URL_NOT_UTF8.load(Ordering::Relaxed),
        COUNTER_ORIGIN_WITH_TAG.load(Ordering::Relaxed),
        total_tags,
        COUNTER_TAG_LIGHTWEIGHT.load(Ordering::Relaxed),
        COUNTER_TAG_ANNOTATED.load(Ordering::Relaxed),
        COUNTER_TAG_UNKNOWN.load(Ordering::Relaxed),
        COUNTER_TAG_NO_TIMESTAMP.load(Ordering::Relaxed),
        COUNTER_INVALID_UTF8_BRANCH_NAME.load(Ordering::Relaxed),
    );

    if !monthly_stats.is_empty() {
        output.push_str("\nMonthly Evolution:\n");
        output.push_str("================\n");
        
        let mut sorted_months: Vec<_> = monthly_stats.iter().collect();
        sorted_months.sort_by_key(|(key, _)| *key);
        
        output.push_str(&format!(
            "{:<12} {:>12} {:>12} {:>12} {:>12}\n",
            "Month", "Lightweight", "Annotated", "Unknown", "Total"
        ));
        output.push_str(&format!("{:-<60}\n", ""));
        
        for (&(year, month), counters) in sorted_months {
            let total = counters.lightweight + counters.annotated + counters.unknown;
            output.push_str(&format!(
                "{:04}-{:02}      {:>12} {:>12} {:>12} {:>12}\n",
                year, month,
                counters.lightweight,
                counters.annotated,
                counters.unknown,
                total
            ));
        }
    }

    output
}
