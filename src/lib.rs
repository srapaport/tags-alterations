use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

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
static COUNTER_REV_NO_TIMESTAMP: AtomicUsize = AtomicUsize::new(0);

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
         Revision without timestamps: {}\n\
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
        COUNTER_REV_NO_TIMESTAMP.load(Ordering::Relaxed),
        COUNTER_TAG_ALTERATION.load(Ordering::Relaxed),
        COUNTER_TAG_REMOVAL.load(Ordering::Relaxed),
    )
}

fn get_tags<G: SwhFullGraph>(
    snapshot: usize,
    graph: &G,
) -> HashMap<String, (usize, i64, Option<usize>)> {
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
                        let rev = revs.pop().unwrap();
                        let root_dir = match swh_graph_stdlib::find_root_dir(graph, rev) {
                            Err(_) => None,
                            Ok(root_dir) => root_dir,
                        };
                        let Some(timestamp) = graph.properties().committer_timestamp(rev) else {
                            COUNTER_REV_NO_TIMESTAMP.fetch_add(1, Ordering::Relaxed);
                            continue;
                        };
                        tags.insert(branch_name, (rev, timestamp, root_dir));
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
    inconsistencies: &mut HashMap<
        String,
        Vec<(
            (String, u64),
            (String, i64, Option<String>),
            (String, u64),
            Option<(String, i64, Option<String>)>,
        )>,
    >,
    current_tags: &HashMap<String, (usize, i64, Option<usize>)>,
    current_snapshot: (usize, u64),
    next_tags: &HashMap<String, (usize, i64, Option<usize>)>,
    next_snapshot: (usize, u64),
    graph: &G,
) {
    let current_snapshot_swhid = graph.properties().swhid(current_snapshot.0).to_string();
    let next_snapshot_swhid = graph.properties().swhid(next_snapshot.0).to_string();
    for (tag_name, &current_rev) in current_tags {
        let current_rev_swhid = graph.properties().swhid(current_rev.0).to_string();
        let current_root_dir_swhid = match current_rev.2 {
            None => None,
            Some(root_dir) => Some(graph.properties().swhid(root_dir).to_string()),
        };
        if let Some(&next_rev) = next_tags.get(tag_name) {
            if next_rev.0 == current_rev.0 {
                continue;
            }
            let next_rev_swhid = graph.properties().swhid(next_rev.0).to_string();
            let next_root_dir_swhid = match next_rev.2 {
                None => None,
                Some(root_dir) => Some(graph.properties().swhid(root_dir).to_string()),
            };
            COUNTER_TAG_ALTERATION.fetch_add(1, Ordering::Relaxed);
            inconsistencies.entry(tag_name.clone()).or_default().push((
                (current_snapshot_swhid.clone(), current_snapshot.1),
                (current_rev_swhid, current_rev.1, current_root_dir_swhid),
                (next_snapshot_swhid.clone(), next_snapshot.1),
                Some((next_rev_swhid, next_rev.1, next_root_dir_swhid)),
            ));
        } else {
            COUNTER_TAG_REMOVAL.fetch_add(1, Ordering::Relaxed);
            inconsistencies.entry(tag_name.clone()).or_default().push((
                (current_snapshot_swhid.clone(), current_snapshot.1),
                (current_rev_swhid, current_rev.1, current_root_dir_swhid),
                (next_snapshot_swhid.clone(), next_snapshot.1),
                None,
            ));
        }
    }
}

fn tags_check_origin<G: SwhFullGraph>(
    origin: usize,
    graph: &G,
) -> Option<
    HashMap<
        String,
        Vec<(
            (String, u64),
            (String, i64, Option<String>),
            (String, u64),
            Option<(String, i64, Option<String>)>,
        )>,
    >,
> {
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
    let (current_snapshot, current_ts) = snapshots_queue.pop_front().unwrap();
    let mut current_snapshot = current_snapshot;
    let mut current_ts = current_ts;
    let mut current_tags = get_tags(current_snapshot, graph);

    for (next_snapshot, next_ts) in snapshots_queue {
        if next_snapshot == current_snapshot {
            continue;
        }
        let next_tags = get_tags(next_snapshot, graph);
        compute_inconsistencies(
            &mut inconsistencies,
            &current_tags,
            (current_snapshot, current_ts),
            &next_tags,
            (next_snapshot, next_ts),
            graph,
        );
        current_snapshot = next_snapshot;
        current_ts = next_ts;
        current_tags = next_tags;
    }
    if inconsistencies.is_empty() {
        return None;
    }
    Some(inconsistencies)
}

pub fn tags_check_full<G: SwhFullGraph + Sync>(
    graph: &G,
    amount_origins: u64,
    suffix: &str,
) -> Result<()> {
    const BATCH_SIZE: usize = 10_000;

    let conn = Connection::open(format!("data/tags_alterations_{}.db", suffix))?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS tag_inconsistencies (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            origin_url TEXT NOT NULL,
            tag_name TEXT NOT NULL,
            old_snapshot TEXT NOT NULL,
            old_snap_timestamp INTEGER NOT NULL,
            old_revision TEXT NOT NULL,
            old_rev_timestamp INTEGER NOT NULL,
            old_root_dir TEXT,
            new_snapshot TEXT NOT NULL,
            new_snap_timestamp INTEGER NOT NULL,
            new_revision TEXT,
            new_rev_timestamp INTEGER,
            new_root_dir TEXT
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

    let (sender, receiver) = mpsc::channel::<(
        String,
        HashMap<
            String,
            Vec<(
                (String, u64),
                (String, i64, Option<String>),
                (String, u64),
                Option<(String, i64, Option<String>)>,
            )>,
        >,
    )>();

    // writer thread
    let db_suffix = suffix.to_string();
    let pb_clone = pb.clone();
    let start_time = Instant::now();
    let writer_handle = thread::spawn(move || -> Result<()> {
        let conn = Connection::open(format!("data/tags_alterations_{}.db", db_suffix))?;
        let mut batch = Vec::with_capacity(BATCH_SIZE);
        let mut batches_written = 0;
        let mut batch_start = Instant::now();

        for result in receiver {
            batch.push(result);

            if batch.len() >= BATCH_SIZE {
                write_batch(&conn, &batch)?;
                let batch_duration = batch_start.elapsed();
                batches_written += 1;
                let total_elapsed = start_time.elapsed();
                pb_clone.println(format!(
                    "✓ Batch {} written ({} origins) - batch: {:.2}s, total: {:.2}s",
                    batches_written,
                    batches_written * BATCH_SIZE,
                    batch_duration.as_secs_f64(),
                    total_elapsed.as_secs_f64()
                ));
                batch.clear();
                batch_start = Instant::now();
            }
        }

        if !batch.is_empty() {
            write_batch(&conn, &batch)?;
            let batch_duration = batch_start.elapsed();
            batches_written += 1;
            let total_elapsed = start_time.elapsed();
            let total_origins = batches_written * BATCH_SIZE - (BATCH_SIZE - batch.len());
            pb_clone.println(format!(
                "✓ Final batch {} written ({} origins) - batch: {:.2}s, total: {:.2}s",
                batches_written,
                total_origins,
                batch_duration.as_secs_f64(),
                total_elapsed.as_secs_f64()
            ));
        }

        Ok(())
    });

    // Process origins in parallel
    (0..graph.num_nodes())
        .into_par_iter()
        .filter(|node| graph.properties().node_type(*node) == NodeType::Origin)
        .filter_map(|origin| {
            pb.inc(1);
            let Some(origin_bytes) = graph.properties().message(origin) else {
                COUNTER_URL_NOT_FOUND.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let Ok(origin_url) = String::from_utf8(origin_bytes) else {
                COUNTER_URL_NOT_UTF8.fetch_add(1, Ordering::Relaxed);
                return None;
            };
            let inconsistencies = tags_check_origin(origin, graph)?;
            Some((origin_url, inconsistencies))
        })
        .for_each_with(sender, |s, result| {
            let _ = s.send(result);
        });

    pb.finish_with_message("Processing complete");

    drop(pb);
    writer_handle.join().expect("Writer thread panicked")?;

    let mut log_file = File::create(format!("data/tags_alterations_{}.log", suffix))?;
    log_file.write_all(format_counters().as_bytes())?;
    display_counters();

    Ok(())
}

fn write_batch(
    conn: &Connection,
    batch: &[(
        String,
        HashMap<
            String,
            Vec<(
                (String, u64),
                (String, i64, Option<String>),
                (String, u64),
                Option<(String, i64, Option<String>)>,
            )>,
        >,
    )],
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO tag_inconsistencies (origin_url, tag_name, old_snapshot, old_snap_timestamp, old_revision, old_rev_timestamp, old_root_dir, new_snapshot, new_snap_timestamp, new_revision, new_rev_timestamp, new_root_dir) 
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;

    let tx = conn.unchecked_transaction()?;
    for (origin, inconsistencies) in batch {
        for (tag_name, alterations) in inconsistencies {
            for (old_snapshot, old_revision, new_snapshot, new_revision) in alterations {
                let (nr, nts, nrd) = match new_revision {
                    None => (None, None, None),
                    Some(elt) => (Some(elt.0.clone()), Some(elt.1), elt.2.clone()),
                };
                stmt.execute(params![
                    origin,
                    tag_name,
                    old_snapshot.0,
                    old_snapshot.1,
                    old_revision.0,
                    old_revision.1,
                    old_revision.2.as_ref(),
                    new_snapshot.0,
                    new_snapshot.1,
                    nr,
                    nts,
                    nrd
                ])?;
            }
        }
    }
    drop(stmt);
    tx.commit()?;
    Ok(())
}
