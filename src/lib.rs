use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rusqlite::{Connection, params};
use swh_graph::labels::VisitStatus;
use swh_graph::{NodeType, graph::*, labels::EdgeLabel};

// Static counters for defensive programming
static COUNTER_ORIGIN_CHECK_ERROR: AtomicUsize = AtomicUsize::new(0);
static COUNTER_VISIT_PARTIAL: AtomicUsize = AtomicUsize::new(0);
static COUNTER_NOT_SNAPSHOT: AtomicUsize = AtomicUsize::new(0);
static COUNTER_INSUFFICIENT_SNAPSHOTS: AtomicUsize = AtomicUsize::new(0);
static COUNTER_INVALID_UTF8_BRANCH_NAME: AtomicUsize = AtomicUsize::new(0);
static COUNTER_NOT_TAG_BRANCH: AtomicUsize = AtomicUsize::new(0);
static COUNTER_NOT_RELEASE: AtomicUsize = AtomicUsize::new(0);
static COUNTER_INVALID_REVS_COUNT: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_ALTERATION: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_REMOVAL: AtomicUsize = AtomicUsize::new(0);
static COUNTER_URL_NOT_FOUND: AtomicUsize = AtomicUsize::new(0);
static COUNTER_URL_NOT_UTF8: AtomicUsize = AtomicUsize::new(0);

pub fn display_counters() {
    println!("{}", format_counters());
}

fn format_counters() -> String {
    format!(
        "\n=== Defensive Programming Counters ===\n\
         Origin check errors: {}\n\
         Partial visits: {}\n\
         Successors not snapshots: {}\n\
         Origins with insufficient snapshots (<2): {}\n\
         Invalid UTF-8 branch names: {}\n\
         Branches not containing '/tags/': {}\n\
         Tag successors not releases: {}\n\
         Releases with invalid revision count (!=1): {}\n\
         Url not found: {}\n\
         Url not utf8: {}\n\
         Tags modified: {}\n\
         Tags deleted: {}\n\
         ======================================\n",
        COUNTER_ORIGIN_CHECK_ERROR.load(Ordering::Relaxed),
        COUNTER_VISIT_PARTIAL.load(Ordering::Relaxed),
        COUNTER_NOT_SNAPSHOT.load(Ordering::Relaxed),
        COUNTER_INSUFFICIENT_SNAPSHOTS.load(Ordering::Relaxed),
        COUNTER_INVALID_UTF8_BRANCH_NAME.load(Ordering::Relaxed),
        COUNTER_NOT_TAG_BRANCH.load(Ordering::Relaxed),
        COUNTER_NOT_RELEASE.load(Ordering::Relaxed),
        COUNTER_INVALID_REVS_COUNT.load(Ordering::Relaxed),
        COUNTER_URL_NOT_FOUND.load(Ordering::Relaxed),
        COUNTER_URL_NOT_UTF8.load(Ordering::Relaxed),
        COUNTER_TAG_ALTERATION.load(Ordering::Relaxed),
        COUNTER_TAG_REMOVAL.load(Ordering::Relaxed),
    )
}

pub fn tags_check_full<G: SwhFullGraph + Sync>(
    graph: &G,
    amount_origins: u64,
    suffix: &str,
) -> Result<()> {
    let conn = Connection::open(format!("data/tags_alterations_{}.db", suffix))?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS tag_inconsistencies (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            origin_url TEXT NOT NULL,
            tag_name TEXT NOT NULL,
            old_snapshot TEXT NOT NULL,
            old_revision TEXT NOT NULL,
            new_snapshot TEXT NOT NULL,
            new_revision TEXT
        )",
        [],
    )?;

    let pb = ProgressBar::new(amount_origins);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} origins ({eta})")
            .unwrap()
            .progress_chars("#>-")
    );

    // Collect all results first (parallel phase)
    let all_results: Vec<_> = (0..graph.num_nodes())
        .into_par_iter()
        .filter(|node| graph.properties().node_type(*node) == NodeType::Origin)
        .filter_map(|origin| {
            let Some(origin_bytes) = graph.properties().message(origin) else {
                COUNTER_URL_NOT_FOUND.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let Ok(origin_url) = String::from_utf8(origin_bytes) else {
                COUNTER_URL_NOT_UTF8.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let inconsistencies = tags_check_origin(origin, graph)?;
            pb.inc(1);
            Some((origin_url, inconsistencies))
        })
        .collect();

    pb.finish_with_message("Processing complete");

    // Batch write to database (single-threaded phase)
    let mut stmt = conn.prepare(
        "INSERT INTO tag_inconsistencies (origin_url, tag_name, old_snapshot, old_revision, new_snapshot, new_revision) 
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;

    let tx = conn.unchecked_transaction()?;
    for (origin, inconsistencies) in all_results {
        for (tag_name, alterations) in inconsistencies {
            for (old_snapshot, old_revision, new_snapshot, new_revision) in alterations {
                stmt.execute(params![
                    origin,
                    tag_name,
                    old_snapshot,
                    old_revision,
                    new_snapshot,
                    new_revision.map(|r| r)
                ])?;
            }
        }
    }
    drop(stmt);
    tx.commit()?;

    let mut log_file = File::create(format!("data/tags_alterations_{}.log", suffix))?;
    log_file.write_all(format_counters().as_bytes())?;
    display_counters();

    Ok(())
}

fn get_tags<G: SwhFullGraph>(snapshot: usize, graph: &G) -> HashMap<String, usize> {
    let mut tags = HashMap::new();
    for (succ, labels) in graph.labeled_successors(snapshot) {
        for label in labels {
            if let EdgeLabel::Branch(branch) = label {
                let Ok(branch_name) =
                    String::from_utf8(graph.properties().label_name(branch.label_name_id()))
                else {
                    COUNTER_INVALID_UTF8_BRANCH_NAME.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                if branch_name.contains("/tags/") {
                    if graph.properties().node_type(succ) != NodeType::Release {
                        COUNTER_NOT_RELEASE.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let mut revs = graph
                        .successors(succ)
                        .into_iter()
                        .filter(|node| graph.properties().node_type(*node) == NodeType::Revision)
                        .collect::<Vec<_>>();
                    if revs.len() == 1 {
                        tags.insert(branch_name, revs.pop().unwrap());
                    } else {
                        COUNTER_INVALID_REVS_COUNT.fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    COUNTER_NOT_TAG_BRANCH.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
    tags
}

fn compute_inconsistencies<G: SwhFullGraph>(
    inconsistencies: &mut HashMap<String, Vec<(String, String, String, Option<String>)>>,
    current_tags: &HashMap<String, usize>,
    current_snapshot: usize,
    next_tags: &HashMap<String, usize>,
    next_snapshot: usize,
    graph: &G,
) {
    let current_snapshot_swhid = graph.properties().swhid(current_snapshot).to_string();
    let next_snapshot_swhid = graph.properties().swhid(next_snapshot).to_string();
    for (tag_name, &current_rev) in current_tags {
        let current_rev_swhid = graph.properties().swhid(current_rev).to_string();
        if let Some(&next_rev) = next_tags.get(tag_name) {
            if next_rev == current_rev {
                continue;
            }
            let next_rev_swhid = graph.properties().swhid(next_rev).to_string();
            COUNTER_TAG_ALTERATION.fetch_add(1, Ordering::Relaxed);
            inconsistencies.entry(tag_name.clone()).or_default().push((
                current_snapshot_swhid.clone(),
                current_rev_swhid,
                next_snapshot_swhid.clone(),
                Some(next_rev_swhid),
            ));
        } else {
            COUNTER_TAG_REMOVAL.fetch_add(1, Ordering::Relaxed);
            inconsistencies.entry(tag_name.clone()).or_default().push((
                current_snapshot_swhid.clone(),
                current_rev_swhid,
                next_snapshot_swhid.clone(),
                None,
            ));
        }
    }
}

fn tags_check_origin<G: SwhFullGraph>(
    origin: usize,
    graph: &G,
) -> Option<HashMap<String, Vec<(String, String, String, Option<String>)>>> {
    let mut snapshots = vec![];
    for (succ, labels) in graph.labeled_successors(origin) {
        if graph.properties().node_type(succ) != NodeType::Snapshot {
            COUNTER_NOT_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        for label in labels {
            if let EdgeLabel::Visit(visit) = label {
                if visit.status() != VisitStatus::Full {
                    COUNTER_VISIT_PARTIAL.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                snapshots.push((succ, visit.timestamp()));
            }
        }
    }
    snapshots.sort_unstable_by_key(|snapshot| snapshot.1);
    let mut snapshots_queue = VecDeque::from_iter(snapshots.into_iter());
    // We need at least 2 snapshots to check tags alterations
    if snapshots_queue.len() < 2 {
        COUNTER_INSUFFICIENT_SNAPSHOTS.fetch_add(1, Ordering::Relaxed);
        return None;
    }

    let mut inconsistencies = HashMap::new();
    let mut current_snapshot = snapshots_queue.pop_front().unwrap().0;
    let mut current_tags = get_tags(current_snapshot, graph);

    for (next_snapshot, _) in snapshots_queue {
        let next_tags = get_tags(next_snapshot, graph);
        compute_inconsistencies(
            &mut inconsistencies,
            &current_tags,
            current_snapshot,
            &next_tags,
            next_snapshot,
            graph,
        );
        current_snapshot = next_snapshot;
        current_tags = next_tags;
    }
    if inconsistencies.is_empty() {
        return None;
    }
    Some(inconsistencies)
}
