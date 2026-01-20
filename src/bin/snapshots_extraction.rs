use anyhow::Result;
use arrow::array::*;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rocksdb::{DB, Options};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, atomic::AtomicUsize};

static COUNTER_NOT_FULL_SNAP: AtomicUsize = AtomicUsize::new(0);
static COUNTER_NOT_GIT_VISIT: AtomicUsize = AtomicUsize::new(0);
static COUNTER_SNAPSHOTS: AtomicUsize = AtomicUsize::new(0);

fn format_counters() -> String {
    format!(
        "\n=== Defensive Programming Counters ===\n\
         Partial visits: {}\n\
         Visit type not `git`: {}\n\
         ======================================\n",
        COUNTER_NOT_FULL_SNAP.load(std::sync::atomic::Ordering::Relaxed),
        COUNTER_NOT_GIT_VISIT.load(std::sync::atomic::Ordering::Relaxed),
    )
}

fn display_counters() {
    println!("{}", format_counters());
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotInfo {
    date_seconds: i64,
    status: String,
    snapshot: Option<String>,
}

fn main() -> Result<()> {
    let orc_dir = "/swh/scratch/graph/2025-10-08/orc/origin_visit_status/";
    let suffix = "full_2025-10";
    let origin_snapshots = Arc::new(Mutex::new(HashMap::<String, Vec<SnapshotInfo>>::new()));

    let entries: Vec<_> = fs::read_dir(orc_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("orc"))
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

    let pb_clone = Arc::clone(&files_pb);

    entries.into_par_iter().for_each(|entry| {
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

    let db_path = format!("data/snapshots_{}_db", suffix);
    let mut opts = Options::default();
    opts.create_if_missing(true);
    let db = Arc::new(DB::open(&opts, &db_path)?);

    println!("Writing to RocksDB...");
    let write_pb = Arc::new(ProgressBar::new(total_origins as u64));
    write_pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "[{elapsed_precise}] {bar:40.magenta/blue} {pos}/{len} origins written ({eta})",
            )
            .unwrap()
            .progress_chars("##-"),
    );

    let pb_clone = Arc::clone(&write_pb);
    let db_clone = Arc::clone(&db);

    origin_snapshots
        .into_par_iter()
        .for_each(|(origin, snapshots)| {
            let value = serde_json::to_vec(&snapshots).expect("Failed to serialize");
            db_clone
                .put(origin.as_bytes(), value)
                .expect("Failed to write to DB");
            pb_clone.inc(1);
        });

    write_pb.finish_with_message("All data written to RocksDB");
    println!("Data written to RocksDB at {}", db_path);

    let mut log_file = File::create(format!("data/snapshots_extraction_{}.log", suffix))?;
    log_file.write_all(format!("Total origins: {}\n", total_origins).as_bytes())?;
    log_file.write_all(format!("Total snapshots: {}\n", total_snapshots).as_bytes())?;
    log_file.write_all(format_counters().as_bytes())?;
    display_counters();
    Ok(())
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
