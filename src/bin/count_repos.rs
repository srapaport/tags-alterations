use anyhow::Result;
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
use tags_alterations::lib_tmp::snapshots_extraction;

static COUNTER_ORIGIN_ANALYZED: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGIN_WITH_SNAPSHOTS: AtomicUsize = AtomicUsize::new(0);
static COUNTER_SNAPSHOTS_PROCESSED: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGIN_NO_VALID_SNAPSHOT: AtomicUsize = AtomicUsize::new(0);

fn main() -> Result<()> {
    dotenv()?;
    // let graph_basename = env::var("GRAPH_BASENAME").expect("GRAPH_BASENAME not set");
    let suffix = env::var("DATASET_SUFFIX").unwrap_or_else(|_| "full_2025-10_v2".to_string());

    // println!("Load graph...");
    // let graph = SwhBidirectionalGraph::new(graph_basename)?
    //     .load_all_properties::<DynMphf>()?
    //     .load_forward_labels()?
    //     .load_backward_labels()?;

    println!("Extract snapshots from ORC files...");
    let origin_snapshots = snapshots_extraction(&suffix)?;
    let total_origins = origin_snapshots.len();
    println!("Extracted {} origins with snapshots", total_origins);

    println!("Start tracking tag changes between snapshots...");
    let pb = Arc::new(ProgressBar::new(total_origins as u64));
    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "{msg} [{bar:40.cyan/blue}] {pos}/{len} origins {percent}% ({per_sec}, {eta})",
            )?
            .progress_chars("#>-"),
    );
    pb.set_message("Processing origins");

    let monthly_origins: HashMap<(i32, u32), usize> = origin_snapshots
        .into_par_iter()
        .filter_map(|(_origin_url, snapshots)| {
            COUNTER_ORIGIN_ANALYZED.fetch_add(1, Ordering::Relaxed);

            let mut local_monthly: HashMap<(i32, u32), usize> = HashMap::new();

            // Get the first snapshot (snapshots are already sorted)
            let Some(first_snapshot) = snapshots.first() else {
                COUNTER_ORIGIN_NO_VALID_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
                pb.inc(1);
                return None;
            };

            COUNTER_ORIGIN_WITH_SNAPSHOTS.fetch_add(1, Ordering::Relaxed);
            COUNTER_SNAPSHOTS_PROCESSED.fetch_add(snapshots.len(), Ordering::Relaxed);

            // Track when this origin was first visited
            let dt = Utc.timestamp_opt(first_snapshot.date_seconds, 0).unwrap();
            let year_month = (dt.year(), dt.month());
            let counter = local_monthly.entry(year_month).or_insert_with(|| 0);
            *counter += 1;

            pb.inc(1);
            Some(local_monthly)
        })
        .reduce(
            || HashMap::new(),
            |mut monthly_a, monthly_b| {
                for ((year, month), counter) in monthly_b {
                    let entry = monthly_a.entry((year, month)).or_insert_with(|| 0);
                    *entry += counter;
                }
                monthly_a
            },
        );

    pb.finish_with_message("Processing complete");

    let output = format_results(&monthly_origins);
    println!("{}", output);

    let mut log_file = File::create(format!("data/counts/commits_count_{}.log", suffix))?;
    log_file.write_all(output.as_bytes())?;

    Ok(())
}

fn format_results(monthly_origins: &HashMap<(i32, u32), usize>) -> String {
    let total_origins = COUNTER_ORIGIN_ANALYZED.load(Ordering::Relaxed);
    let origins_with_snapshots = COUNTER_ORIGIN_WITH_SNAPSHOTS.load(Ordering::Relaxed);
    let total_snapshots = COUNTER_SNAPSHOTS_PROCESSED.load(Ordering::Relaxed);
    let origins_no_snapshots = COUNTER_ORIGIN_NO_VALID_SNAPSHOT.load(Ordering::Relaxed);

    let mut output = format!(
        "\n\nOrigin First Visit Counting Results\n\
        =====================================\n\
        Analyzed {} origins\n\
        Origins with valid snapshots: {}\n\
        Origins without valid snapshots: {}\n\
        Total snapshots in dataset: {}\n\n",
        total_origins, origins_with_snapshots, origins_no_snapshots, total_snapshots,
    );

    if !monthly_origins.is_empty() {
        output.push_str("\nMonthly Evolution (Origins by First Visit):\n");
        output.push_str("===========================================\n");

        let mut sorted_months: Vec<_> = monthly_origins.iter().collect();
        sorted_months.sort_by_key(|(key, _)| *key);

        output.push_str(&format!(
            "{:<12} {:>15} {:>15}\n",
            "Month", "New Origins", "Cumulative"
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
        output.push_str(&format!("\nTotal origins tracked: {}\n", cumulative));
    }

    output
}
