use anyhow::Result;
use arrow::array::*;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
//use rocksdb::DB;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;
use swh_graph::{NodeType, graph::*, labels::EdgeLabel};

// Static counters for defensive programming
static COUNTER_SNAPSHOT_NOT_IN_GRAPH: AtomicUsize = AtomicUsize::new(0);
static COUNTER_INSUFFICIENT_SNAPSHOTS: AtomicUsize = AtomicUsize::new(0);
static COUNTER_INVALID_UTF8_BRANCH_NAME: AtomicUsize = AtomicUsize::new(0);
static COUNTER_NOT_TAG_BRANCH: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_LIGHTWEIGHT: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_ANNOTATED: AtomicUsize = AtomicUsize::new(0);
static COUNTER_INVALID_REVS_COUNT: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_ALTERATION: AtomicUsize = AtomicUsize::new(0);
static COUNTER_TAG_REMOVAL: AtomicUsize = AtomicUsize::new(0);
static COUNTER_REV_NO_TIMESTAMP: AtomicUsize = AtomicUsize::new(0);
static COUNTER_DESERIALIZATION_ERROR: AtomicUsize = AtomicUsize::new(0);
static COUNTER_NOT_FULL_SNAP: AtomicUsize = AtomicUsize::new(0);
static COUNTER_NOT_GIT_VISIT: AtomicUsize = AtomicUsize::new(0);
static COUNTER_SNAPSHOTS: AtomicUsize = AtomicUsize::new(0);
static COUNTER_INVALID_SUCCESSOR: AtomicUsize = AtomicUsize::new(0);

fn format_counters_bis() -> String {
    format!(
        "\n=== Programming Counters Bis ===\n\
         Partial visits: {}\n\
         Visit type not `git`: {}\n\
         ======================================\n",
        COUNTER_NOT_FULL_SNAP.load(std::sync::atomic::Ordering::Relaxed),
        COUNTER_NOT_GIT_VISIT.load(std::sync::atomic::Ordering::Relaxed),
    )
}

fn display_counters_bis() {
    println!("{}", format_counters_bis());
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SnapshotInfo {
    pub date_seconds: i64,
    pub status: String,
    pub snapshot: Option<String>,
}

pub fn display_counters() {
    println!("{}", format_counters());
}

fn format_counters() -> String {
    format!(
        "\n=== Programming Counters ===\n\
         Snapshots not found in graph: {}\n\
         Origins with insufficient snapshots (<2): {}\n\
         Invalid UTF-8 branch names: {}\n\
         Branches not containing '/tags/': {}\n\
         Lightweight tags: {}\n\
         Annotated tags: {}\n\
         Releases with invalid revision count (!=1): {}\n\
         Revision without timestamps: {}\n\
         Deserialization errors: {}\n\
         Invalid Snapshots successor: {}\n\
         Tags modified: {}\n\
         Tags deleted: {}\n\
         ======================================\n",
        COUNTER_SNAPSHOT_NOT_IN_GRAPH.load(Ordering::Relaxed),
        COUNTER_INSUFFICIENT_SNAPSHOTS.load(Ordering::Relaxed),
        COUNTER_INVALID_UTF8_BRANCH_NAME.load(Ordering::Relaxed),
        COUNTER_NOT_TAG_BRANCH.load(Ordering::Relaxed),
        COUNTER_TAG_LIGHTWEIGHT.load(Ordering::Relaxed),
        COUNTER_TAG_ANNOTATED.load(Ordering::Relaxed),
        COUNTER_INVALID_REVS_COUNT.load(Ordering::Relaxed),
        COUNTER_REV_NO_TIMESTAMP.load(Ordering::Relaxed),
        COUNTER_DESERIALIZATION_ERROR.load(Ordering::Relaxed),
        COUNTER_INVALID_SUCCESSOR.load(Ordering::Relaxed),
        COUNTER_TAG_ALTERATION.load(Ordering::Relaxed),
        COUNTER_TAG_REMOVAL.load(Ordering::Relaxed),
    )
}

fn get_tags<G: SwhFullGraph>(
    snapshot: usize,
    snap_timestamp: u64,
    count_snapshot: u64,
    graph: &G,
) -> HashMap<(String, String), (Option<i64>, usize, i64, Option<usize>, u64, usize, u64)> {
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
                    match graph.properties().node_type(succ) {
                        NodeType::Revision => {
                            COUNTER_TAG_LIGHTWEIGHT.fetch_add(1, Ordering::Relaxed);
                            insert_tag(
                                &mut tags,
                                None,
                                succ,
                                branch_name,
                                String::from("lightweight"),
                                count_snapshot,
                                snapshot,
                                snap_timestamp,
                                graph,
                            );
                        }
                        NodeType::Release => {
                            COUNTER_TAG_ANNOTATED.fetch_add(1, Ordering::Relaxed);
                            let successors = graph.successors(succ).into_iter().collect::<Vec<_>>();
                            let mut revs = successors
                                .iter()
                                .copied()
                                .filter(|node| {
                                    graph.properties().node_type(*node) == NodeType::Revision
                                })
                                .collect::<Vec<_>>();
                            if revs.is_empty() {
                                let releases: Vec<_> = successors
                                    .iter()
                                    .copied()
                                    .filter(|node| {
                                        graph.properties().node_type(*node) == NodeType::Release
                                    })
                                    .collect();

                                if releases.len() == 1 {
                                    revs = graph
                                        .successors(releases[0])
                                        .into_iter()
                                        .filter(|node| {
                                            graph.properties().node_type(*node)
                                                == NodeType::Revision
                                        })
                                        .collect();
                                }
                            }
                            if revs.len() == 1 {
                                let rev = revs.pop().unwrap();
                                let tag_timestamp = graph.properties().author_timestamp(succ);
                                insert_tag(
                                    &mut tags,
                                    tag_timestamp,
                                    rev,
                                    branch_name,
                                    String::from("annotated"),
                                    count_snapshot,
                                    snapshot,
                                    snap_timestamp,
                                    graph,
                                );
                            } else {
                                COUNTER_INVALID_REVS_COUNT.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        _ => {
                            COUNTER_INVALID_SUCCESSOR.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                } else {
                    COUNTER_NOT_TAG_BRANCH.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
    tags
}

fn insert_tag<G: SwhFullGraph>(
    tags: &mut HashMap<(String, String), (Option<i64>, usize, i64, Option<usize>, u64, usize, u64)>,
    tag_timestamp: Option<i64>,
    rev: usize,
    branch_name: String,
    tag_type: String,
    count_snapshot: u64,
    snapshot: usize,
    snap_timestamp: u64,
    graph: &G,
) {
    let root_dir = match swh_graph_stdlib::find_root_dir(graph, rev) {
        Err(_) => None,
        Ok(root_dir) => root_dir,
    };
    let Some(timestamp) = graph.properties().committer_timestamp(rev) else {
        COUNTER_REV_NO_TIMESTAMP.fetch_add(1, Ordering::Relaxed);
        return;
    };
    tags.insert(
        (branch_name, tag_type),
        (
            tag_timestamp,
            rev,
            timestamp,
            root_dir,
            count_snapshot,
            snapshot,
            snap_timestamp,
        ),
    );
}

fn compute_inconsistencies<G: SwhFullGraph>(
    inconsistencies: &mut HashMap<
        (String, String),
        Vec<(
            (u64, u64, u64),
            (String, u64),
            (String, i64, Option<String>),
            (String, u64),
            Option<(String, i64, Option<String>)>,
        )>,
    >,
    count_snapshot: u64,
    min_delta: u64,
    cumulative_tags: &mut HashMap<(String, String), (Option<i64>, usize, i64, Option<usize>, u64, usize, u64)>,
    //current_snapshot: (usize, u64),
    next_tags: HashMap<(String, String), (Option<i64>, usize, i64, Option<usize>, u64, usize, u64)>,
    next_snapshot: (usize, u64),
    graph: &G,
) {
    let next_snapshot_swhid = graph.properties().swhid(next_snapshot.0).to_string();
    for (tag_name, current_tag) in cumulative_tags.clone() {
        let current_snapshot_swhid = graph.properties().swhid(current_tag.5).to_string();
        if let Some(&next_tag) = next_tags.get(&tag_name) {
            if next_tag.1 == current_tag.1 {
                continue;
            }
            let current_rev_swhid = graph.properties().swhid(current_tag.1).to_string();
            let current_root_dir_swhid = current_tag
                .3
                .map(|root_dir| graph.properties().swhid(root_dir).to_string());
            let next_rev_swhid = graph.properties().swhid(next_tag.1).to_string();
            let next_root_dir_swhid = next_tag
                .3
                .map(|root_dir| graph.properties().swhid(root_dir).to_string());

            COUNTER_TAG_ALTERATION.fetch_add(1, Ordering::Relaxed);
            inconsistencies.entry(tag_name.clone()).or_default().push((
                (current_tag.4, next_tag.4, min_delta),
                (current_snapshot_swhid.clone(), current_tag.6),
                (current_rev_swhid, current_tag.2, current_root_dir_swhid),
                (next_snapshot_swhid.clone(), next_snapshot.1),
                Some((next_rev_swhid, next_tag.2, next_root_dir_swhid)),
            ));
            cumulative_tags.insert(tag_name.clone(), next_tag);
        } else {
            let current_rev_swhid = graph.properties().swhid(current_tag.1).to_string();
            let current_root_dir_swhid = current_tag
                .3
                .map(|root_dir| graph.properties().swhid(root_dir).to_string());

            COUNTER_TAG_REMOVAL.fetch_add(1, Ordering::Relaxed);
            inconsistencies.entry(tag_name.clone()).or_default().push((
                (current_tag.4, count_snapshot, min_delta),
                (current_snapshot_swhid.clone(), current_tag.6),
                (current_rev_swhid, current_tag.2, current_root_dir_swhid),
                (next_snapshot_swhid.clone(), next_snapshot.1),
                None,
            ));
            cumulative_tags.remove(&tag_name);
        }
    }

    for (tag_name, next_tag) in next_tags {
        if !cumulative_tags.contains_key(&tag_name) {
            cumulative_tags.insert(tag_name, next_tag);
        }
    }
}

fn tags_check_origin<G: SwhFullGraph>(
    snapshot_infos: Vec<SnapshotInfo>,
    graph: &G,
) -> Option<
    HashMap<
        (String, String),
        Vec<(
            (u64, u64, u64),
            (String, u64),
            (String, i64, Option<String>),
            (String, u64),
            Option<(String, i64, Option<String>)>,
        )>,
    >,
> {
    let mut snapshots = vec![];
    for info in snapshot_infos {
        let Some(snapshot_swhid) = info.snapshot else {
            continue;
        };
        //println!("snapshot swhid: {}", snapshot_swhid);
        let Ok(snapshot_node) = graph
            .properties()
            .node_id(format!("swh:1:snp:{}", snapshot_swhid).as_str())
        else {
            COUNTER_SNAPSHOT_NOT_IN_GRAPH.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        snapshots.push((snapshot_node, info.date_seconds as u64));
    }

    if snapshots.len() < 2 {
        COUNTER_INSUFFICIENT_SNAPSHOTS.fetch_add(1, Ordering::Relaxed);
        return None;
    }

    let mut count_snapshot = 0;
    let mut inconsistencies = HashMap::new();
    let mut snapshots_iter = snapshots.into_iter();
    let (mut current_snapshot, current_ts) = snapshots_iter.next().unwrap();
    let mut min_delta = current_ts;
    let mut cumulative_tags = get_tags(current_snapshot, current_ts, count_snapshot, graph);

    for (next_snapshot, next_ts) in snapshots_iter {
        count_snapshot += 1;
        if next_snapshot == current_snapshot {
            min_delta = next_ts;
            continue;
        }
        let next_tags = get_tags(next_snapshot, next_ts, count_snapshot, graph);
        compute_inconsistencies(
            &mut inconsistencies,
            count_snapshot,
            min_delta,
            &mut cumulative_tags,
            //(current_snapshot, current_ts),
            next_tags,
            (next_snapshot, next_ts),
            graph,
        );
        current_snapshot = next_snapshot;
        //current_ts = next_ts;
        min_delta = next_ts;
    }
    if inconsistencies.is_empty() {
        return None;
    }
    Some(inconsistencies)
}

pub fn tags_check_full<G: SwhFullGraph + Sync>(
    graph: &G,
    suffix: &str,
    orc_dir: &str,
    db_path: &str,
) -> Result<()> {
    const DB_BATCH_SIZE: usize = 10_000;

    let snapshots = snapshots_extraction_with_dir(orc_dir, suffix)?;
    let total_origins = snapshots.len();
    println!("Total origins in RocksDB: {}", total_origins);

    let conn = Connection::open(db_path)?;

    // Checkpointing
    let table_exists = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='tag_inconsistencies'")
        .and_then(|mut stmt| stmt.exists([]))
        .unwrap_or(false);

    let processed_origins = if table_exists {
        println!("Found existing database, loading checkpoint...");
        let mut stmt = conn.prepare("SELECT DISTINCT origin_url FROM tag_inconsistencies")?;
        let origins: std::collections::HashSet<String> = stmt
            .query_map([], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        println!(
            "Checkpoint loaded: {} origins already processed",
            origins.len()
        );
        Arc::new(origins)
    } else {
        println!("No checkpoint found, starting fresh");
        conn.execute(
            "CREATE TABLE IF NOT EXISTS tag_inconsistencies (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                origin_url TEXT NOT NULL,
                tag_name TEXT NOT NULL,
                type TEXT NOT NULL,
                old_snapshot TEXT NOT NULL,
                old_snapshot_cpt INTEGER NOT NULL,
                old_snap_timestamp INTEGER NOT NULL,
                old_revision TEXT NOT NULL,
                old_rev_timestamp INTEGER NOT NULL,
                old_root_dir TEXT,
                new_snapshot TEXT NOT NULL,
                new_snapshot_cpt INTEGER NOT NULL,
                new_snap_timestamp INTEGER NOT NULL,
                new_revision TEXT,
                new_rev_timestamp INTEGER,
                new_root_dir TEXT,
                min_delta INTEGER NOT NULL
            )",
            [],
        )?;
        Arc::new(std::collections::HashSet::new())
    };

    drop(conn);

    let pb = ProgressBar::new(total_origins as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} origins ({eta})")
            .unwrap()
            .progress_chars("#>-")
    );

    let start_time = Instant::now();

    let (work_sender, work_receiver) =
        crossbeam_channel::unbounded::<(String, Vec<SnapshotInfo>)>();
    let (result_sender, result_receiver) = crossbeam_channel::unbounded();

    // Writer thread
    let db_path = db_path.to_string();
    let writer_handle = thread::spawn(move || -> Result<()> {
        let conn = Connection::open(db_path)?;
        let mut write_batch_buffer = Vec::with_capacity(DB_BATCH_SIZE);
        let mut total_written = 0;
        println!("Writer worker on standby, ready to write");
        for result in result_receiver {
            write_batch_buffer.push(result);

            if write_batch_buffer.len() >= DB_BATCH_SIZE {
                write_batch(&conn, &write_batch_buffer)?;
                total_written += write_batch_buffer.len();
                println!(
                    "✓ Written {} origins - {:.2}s",
                    total_written,
                    start_time.elapsed().as_secs_f64()
                );
                write_batch_buffer.clear();
            }
        }

        if !write_batch_buffer.is_empty() {
            write_batch(&conn, &write_batch_buffer)?;
            total_written += write_batch_buffer.len();
            println!(
                "✓ Final: {} origins - {:.2}s",
                total_written,
                start_time.elapsed().as_secs_f64()
            );
        }
        Ok(())
    });

    let num_workers = rayon::current_num_threads() / 2;
    //println!("Starting {} worker threads", num_workers);

    std::thread::scope(|s| {
        println!("Spawning {} workers (compute inconsistencies)", num_workers);
        for _ in 0..num_workers {
            let work_rx = work_receiver.clone();
            let result_tx = result_sender.clone();
            let pb = pb.clone();

            s.spawn(move || {
                for (origin_url, snapshot_infos) in work_rx {
                    if let Some(inconsistencies) = tags_check_origin(snapshot_infos, graph) {
                        let _ = result_tx.send((origin_url, inconsistencies));
                    }
                    pb.inc(1);
                }
            });
        }

        let mut skipped_count = 0;
        println!("Feeding the workers");
        for (origin_url, snapshot_infos) in snapshots {
            if processed_origins.contains(&origin_url) {
                skipped_count += 1;
                pb.inc(1);
                continue;
            }
            work_sender.send((origin_url, snapshot_infos)).unwrap();
        }

        println!("Skipped {} already processed origins", skipped_count);

        drop(work_sender);
    });

    pb.finish_with_message("Processing complete");

    drop(result_sender);
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
            (String, String),
            Vec<(
                (u64, u64, u64),
                (String, u64),
                (String, i64, Option<String>),
                (String, u64),
                Option<(String, i64, Option<String>)>,
            )>,
        >,
    )],
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO tag_inconsistencies (origin_url, tag_name, type, old_snapshot, old_snapshot_cpt, old_snap_timestamp, old_revision, old_rev_timestamp, old_root_dir, new_snapshot, new_snapshot_cpt, new_snap_timestamp, new_revision, new_rev_timestamp, new_root_dir, min_delta) 
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
    )?;

    let tx = conn.unchecked_transaction()?;
    for (origin, inconsistencies) in batch {
        for (tag_name, alterations) in inconsistencies {
            for (min_delta, old_snapshot, old_revision, new_snapshot, new_revision) in alterations {
                let (nr, nts, nrd) = match new_revision {
                    None => (None, None, None),
                    Some(elt) => (Some(elt.0.clone()), Some(elt.1), elt.2.clone()),
                };
                stmt.execute(params![
                    origin,
                    tag_name.0,
                    tag_name.1,
                    old_snapshot.0,
                    min_delta.0,
                    old_snapshot.1,
                    old_revision.0,
                    old_revision.1,
                    old_revision.2.as_ref(),
                    new_snapshot.0,
                    min_delta.1,
                    new_snapshot.1,
                    nr,
                    nts,
                    nrd,
                    min_delta.2
                ])?;
            }
        }
    }
    drop(stmt);
    tx.commit()?;
    Ok(())
}

pub fn snapshots_extraction(suffix: &str) -> Result<HashMap<String, Vec<SnapshotInfo>>> {
    snapshots_extraction_for_suffix(suffix)
}

pub fn snapshots_extraction_for_suffix(suffix: &str) -> Result<HashMap<String, Vec<SnapshotInfo>>> {
    let orc_dir = default_orc_dir_for_suffix(suffix)?;
    snapshots_extraction_with_dir(orc_dir, suffix)
}

pub fn snapshots_extraction_with_dir(orc_dir: &str, suffix: &str) -> Result<HashMap<String, Vec<SnapshotInfo>>> {
    let origin_snapshots = Arc::new(Mutex::new(HashMap::<String, Vec<SnapshotInfo>>::new()));

    let entries: Vec<_> = fs::read_dir(orc_dir)?
        .filter_map(|e| e.ok())
        //.filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("orc"))
        .collect();

    let total_files = entries.len();
    println!("Found {} ORC files to process", total_files);

    let files_pb = Arc::new(ProgressBar::new(total_files as u64));
    files_pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} files ({eta})")
            .unwrap()
            .progress_chars("##-"),
    );
    files_pb.tick();

    entries.into_par_iter().for_each(|entry| {
        let pb_clone = Arc::clone(&files_pb);
        let path = entry.path();
        match process_orc_file(&path) {
            Ok(local_map) => {
                let mut global_map = origin_snapshots.lock().unwrap();
                for (origin, mut snapshots) in local_map {
                    global_map
                        .entry(origin)
                        .or_insert_with(Vec::new)
                        .append(&mut snapshots);
                }
            }
            Err(e) => {
                eprintln!("Error processing {:?}: {}", path, e);
            }
        }
        pb_clone.inc(1);
    });

    files_pb.finish_with_message("All files processed");

    let mut origin_snapshots = Arc::try_unwrap(origin_snapshots)
        .unwrap()
        .into_inner()
        .unwrap();

    let total_origins = origin_snapshots.len();
    println!(
        "Total origins with at least one *FULL* *git* visit: {}",
        total_origins
    );
    let sort_pb = ProgressBar::new(origin_snapshots.len() as u64);
    sort_pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.magenta/blue} {pos}/{len} origins with sorted snapshots ({eta})")
            .unwrap()
            .progress_chars("##-"),
    );
    for snapshots in origin_snapshots.values_mut() {
        snapshots.sort_by_key(|s| s.date_seconds);
        sort_pb.inc(1);
    }
    sort_pb.finish_with_message("Done sorting snapshots");

    let total_snapshots = COUNTER_SNAPSHOTS.load(std::sync::atomic::Ordering::Relaxed);

    println!("Total origins: {}", total_origins);
    println!("Total snapshots: {}", total_snapshots);

    let mut log_file = File::create(format!("data/snapshots_extraction_{}.log", suffix))?;
    log_file.write_all(format!("Total origins: {}\n", total_origins).as_bytes())?;
    log_file.write_all(format!("Total snapshots: {}\n", total_snapshots).as_bytes())?;
    log_file.write_all(format_counters_bis().as_bytes())?;
    display_counters_bis();
    Ok(origin_snapshots)
}

fn default_orc_dir_for_suffix(suffix: &str) -> Result<&'static str> {
    match suffix {
        "full_2025-10" | "full_2025-10_v2" | "test" => {
            Ok("/swh/scratch/graph/2025-10-08/orc/origin_visit_status/")
        }
        "teaser_2025-05" => Err(anyhow::anyhow!(
            "No default ORC directory for teaser_2025-05. Provide --orc-dir or ORC_DIR"
        )),
        _ => Err(anyhow::anyhow!(
            "unknown dataset suffix; provide ORC_DIR and call snapshots_extraction_with_dir"
        )),
    }
}

fn process_orc_file(path: &Path) -> Result<HashMap<String, Vec<SnapshotInfo>>> {
    let mut local_snapshots: HashMap<String, Vec<SnapshotInfo>> = HashMap::new();

    let file = fs::File::open(path)?;
    let reader = orc_rust::ArrowReaderBuilder::try_new(file)?.build();
    let record_batches = reader.collect::<Result<Vec<_>, _>>()?;

    for record_batch in record_batches {
        let num_rows = record_batch.num_rows();

        let origin_col = record_batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let date_col = record_batch
            .column(2)
            .as_any()
            .downcast_ref::<TimestampNanosecondArray>()
            .unwrap();
        let status_col = record_batch
            .column(3)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let snapshot_col = record_batch
            .column(4)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let type_col = record_batch
            .column(5)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();

        for row_idx in 0..num_rows {
            let status = status_col.value(row_idx).to_string();
            if status != "full" {
                COUNTER_NOT_FULL_SNAP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                continue;
            }

            let type_val = type_col.value(row_idx);
            if type_val != "git" {
                COUNTER_NOT_GIT_VISIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                continue;
            }

            let origin = origin_col.value(row_idx).to_string();
            let date_nanos = date_col.value(row_idx);

            let snapshot = if snapshot_col.is_null(row_idx) {
                None
            } else {
                Some(snapshot_col.value(row_idx).to_string())
            };

            let date_seconds = date_nanos / 1_000_000_000;

            let info = SnapshotInfo {
                date_seconds,
                status,
                snapshot,
            };
            COUNTER_SNAPSHOTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            local_snapshots
                .entry(origin)
                .or_insert_with(Vec::new)
                .push(info);
        }
    }

    Ok(local_snapshots)
}
