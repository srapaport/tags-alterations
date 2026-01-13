use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

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
        COUNTER_TAG_ALTERATION.load(Ordering::Relaxed),
        COUNTER_TAG_REMOVAL.load(Ordering::Relaxed),
    )
}

pub fn tags_check_full<G: SwhFullGraph + Sync>(graph: &G, amount_origins: u64) -> Result<()> {
    let conn = Connection::open("tags_alterations.db")?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS tag_inconsistencies (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            origin_node INTEGER NOT NULL,
            tag_name TEXT NOT NULL,
            old_revision INTEGER NOT NULL,
            new_revision INTEGER
        )",
        [],
    )?;
    
    let conn = Mutex::new(conn);
    
    let pb = ProgressBar::new(amount_origins);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} origins ({eta})")
            .unwrap()
            .progress_chars("#>-")
    );
    
    (0..graph.num_nodes())
        .into_par_iter()
        .filter(|node| graph.properties().node_type(*node) == NodeType::Origin)
        .for_each(|origin| {
            let Some(inconsistencies) = tags_check_origin(origin, graph) else {
                pb.inc(1);
                return;
            };
            
            // Write inconsistencies to the database
            if !inconsistencies.is_empty() {
                let conn = conn.lock().unwrap();
                for (tag_name, alterations) in inconsistencies {
                    for (old_revision, new_revision) in alterations {
                        let _ = conn.execute(
                            "INSERT INTO tag_inconsistencies (origin_node, tag_name, old_revision, new_revision) 
                             VALUES (?1, ?2, ?3, ?4)",
                            params![origin, tag_name, old_revision as i64, new_revision.map(|r| r as i64)],
                        );
                    }
                }
            }
            
            pb.inc(1);
        });
    
    pb.finish_with_message("Processing complete");
    
    let mut log_file = File::create("tags_alterations.log")?;
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

fn compute_inconsistencies(
    inconsistencies: &mut HashMap<String, Vec<(usize, Option<usize>)>>,
    current_tags: &HashMap<String, usize>,
    next_tags: &HashMap<String, usize>,
) {
    for current_tag in current_tags {
        if let Some(next_tag) = next_tags.get(current_tag.0) {
            if *next_tag == *current_tag.1 {
                continue;
            }
            COUNTER_TAG_ALTERATION.fetch_add(1, Ordering::Relaxed);
            inconsistencies
                .entry(current_tag.0.clone())
                .or_insert_with(|| Vec::new())
                .push((*current_tag.1, Some(*next_tag)));
        } else {
            COUNTER_TAG_REMOVAL.fetch_add(1, Ordering::Relaxed);
            inconsistencies
                .entry(current_tag.0.clone())
                .or_insert_with(|| Vec::new())
                .push((*current_tag.1, None));
        }
    }
}

fn tags_check_origin<G: SwhFullGraph>(
    origin: usize,
    graph: &G,
) -> Option<HashMap<String, Vec<(usize, Option<usize>)>>> {
    let mut snapshots = vec![];
    for (succ, labels) in graph.labeled_successors(origin) {
        if graph.properties().node_type(succ) != NodeType::Snapshot {
            COUNTER_NOT_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        for label in labels{
            if let EdgeLabel::Visit(visit) = label{
                if visit.status() != VisitStatus::Full{
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
    let current_snapshot = snapshots_queue.pop_front().unwrap();
    let mut current_tags = get_tags(current_snapshot.0, graph);
    while let Some(next_snapshot) = snapshots_queue.pop_front() {
        let next_tags = get_tags(next_snapshot.0, graph);
        compute_inconsistencies(&mut inconsistencies, &current_tags, &next_tags);
        current_tags = next_tags;
    }
    Some(inconsistencies)
}
