use anyhow::{Result, anyhow};
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

static COUNTER_INVALID_UTF8_BRANCH_NAME: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_LIGHTWEIGHT: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_ANNOTATED: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_UNKNOWN: AtomicUsize = AtomicUsize::new(0);
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
    let graph = SwhBidirectionalGraph::new(graph_basename)?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;

    // let mut tags = HashMap::new();
    // //let conn = Connection::open(format!("data/tags_alterations_{}.db", suffix))?;
    // (0..graph.num_nodes())
    //     .into_par_iter()
    //     .filter(|node| graph.properties().node_type(*node) == NodeType::Origin)
    //     .for_each(|origin| {
    //         graph
    //             .successors(origin)
    //             .filter(|snapshot| graph.properties().node_type(*snapshot) == NodeType::Snapshot)
    //             .for_each(|snapshot| {
    //                 graph
    //                     .labeled_successors(snapshot)
    //                     .filter(|(succ, _)| {
    //                         [NodeType::Release, NodeType::Revision]
    //                             .contains(&graph.properties().node_type(*succ))
    //                     })
    //                     .for_each(|(rel, labels)| {
    //                         let Some(origin_bytes) = graph.properties().message(origin) else {
    //                             return;
    //                         };
    //                         let Ok(origin_url) = String::from_utf8(origin_bytes) else {
    //                             return;
    //                         };
    //                         for label in labels {
    //                             if let EdgeLabel::Branch(branch) = label {
    //                                 let Ok(branch_name) = String::from_utf8(
    //                                     graph.properties().label_name(branch.label_name_id()),
    //                                 ) else {
    //                                     COUNTER_INVALID_UTF8_BRANCH_NAME
    //                                         .fetch_add(1, Ordering::Relaxed);
    //                                     continue;
    //                                 };
    //                                 if branch_name.contains("/tags/") {
    //                                     tags.entry(origin_url.clone())
    //                                         .or_insert_with(Vec::new)
    //                                         .push((
    //                                             branch_name,
    //                                             match graph.properties().node_type(rel) {
    //                                                 NodeType::Release => "annotated",
    //                                                 NodeType::Revision => "lightweight",
    //                                                 _ => "unknown",
    //                                             },
    //                                         ))
    //                                 }
    //                             }
    //                         }
    //                     });
    //             });
    //     });

    let pb = Arc::new(ProgressBar::new(amount_origins));
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{msg} [{bar:40.cyan/blue}] {pos}/{len} origins ({per_sec}, {eta})")?
            .progress_chars("#>-"),
    );
    pb.set_message("Collecting tags");

    let tags: HashMap<_, _> = (0..graph.num_nodes())
        .into_par_iter()
        .filter(|node| graph.properties().node_type(*node) == NodeType::Origin)
        .map(|origin| {
            let pb = Arc::clone(&pb);
            let mut origin_url_cache: Option<String> = None;
            let mut local_tags: HashMap<String, &str> = HashMap::new();

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
                                        let tag_type = match graph.properties().node_type(rel) {
                                            NodeType::Release => "annotated",
                                            NodeType::Revision => "lightweight",
                                            _ => "unknown",
                                        };
                                        if local_tags.insert(branch_name, tag_type) == None {
                                            match tag_type {
                                                "annotated" => COUNTER_TAG_ANNOTATED
                                                    .fetch_add(1, Ordering::Relaxed),
                                                "lightweight" => COUNTER_TAG_LIGHTWEIGHT
                                                    .fetch_add(1, Ordering::Relaxed),
                                                _ => COUNTER_TAG_UNKNOWN
                                                    .fetch_add(1, Ordering::Relaxed),
                                            };
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
            (origin_url_cache, local_tags)
        })
        .fold(HashMap::new, |mut acc, (url, tag)| {
            if let Some(origin) = url {
                COUNTER_ORIGIN_ANALYZED.fetch_add(1, Ordering::Relaxed);
                if !tag.is_empty() {
                    COUNTER_ORIGIN_WITH_TAG.fetch_add(1, Ordering::Relaxed);
                }
                acc.entry(origin).or_insert_with(HashMap::new).extend(tag);
            } else {
                COUNTER_ORIGIN_URL_NOT_UTF8.fetch_add(1, Ordering::Relaxed);
            }
            acc
        })
        .reduce(HashMap::new, |mut a, mut b| {
            if a.len() < b.len() {
                std::mem::swap(&mut a, &mut b);
            }
            for (k, v) in b {
                a.entry(k).or_insert_with(HashMap::new).extend(v);
            }
            a
        });

    pb.finish_with_message("Tags collected");

    let total_tags: usize = tags.values().map(|v| v.len()).sum();
    println!("{}", format_counters(total_tags));

    let mut log_file = File::create("data/tags_count.log")?;
    log_file.write_all(format_counters(total_tags).as_bytes())?;

    // Write results to SQLite database
    // let write_pb = ProgressBar::new(total_tags as u64);
    // write_pb.set_style(
    //     ProgressStyle::default_bar()
    //         .template("{msg} [{bar:40.green/blue}] {pos}/{len} tags ({per_sec}, {eta})")?
    //         .progress_chars("#>-"),
    // );
    // write_pb.set_message("Writing to database");

    // let conn = Connection::open("data/tags_count.db")?;

    // conn.execute(
    //     "CREATE TABLE IF NOT EXISTS tags (
    //         origin_url TEXT NOT NULL,
    //         tag_name TEXT NOT NULL,
    //         type TEXT NOT NULL
    //     )",
    //     [],
    // )?;

    // conn.execute("DELETE FROM tags", [])?;

    // let tx = conn.unchecked_transaction()?;
    // {
    //     let mut stmt =
    //         tx.prepare("INSERT INTO tags (origin_url, tag_name, type) VALUES (?, ?, ?)")?;

    //     for (origin_url, tag_map) in tags {
    //         for (tag_name, tag_type) in tag_map {
    //             stmt.execute(params![origin_url, tag_name, tag_type])?;
    //             write_pb.inc(1);
    //         }
    //     }
    // }
    // tx.commit()?;

    // write_pb.finish_with_message("Done writing to data/tags_count.db");

    Ok(())
}

fn format_counters(total_tags: usize) -> String {
    format!(
        "\n\nAnalized {} origins | Skipped {} origins\n
        Collected {} origins with {} total tags\n
          - Lightweight tags: {}\n
          - Annotated tags: {}\n
          - Unknown tags: {}\n
          - Unknown branch name (not utf8): {}\n",
        COUNTER_ORIGIN_ANALYZED.load(Ordering::Relaxed),
        COUNTER_ORIGIN_URL_NOT_UTF8.load(Ordering::Relaxed),
        COUNTER_ORIGIN_WITH_TAG.load(Ordering::Relaxed),
        total_tags,
        COUNTER_TAG_LIGHTWEIGHT.load(Ordering::Relaxed),
        COUNTER_TAG_ANNOTATED.load(Ordering::Relaxed),
        COUNTER_TAG_UNKNOWN.load(Ordering::Relaxed),
        COUNTER_INVALID_UTF8_BRANCH_NAME.load(Ordering::Relaxed),
    )
}
