use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::Connection;
use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use tags_alterations::lib_tmp::snapshots_extraction;

static COUNTER_SQL_ROWS_READ: AtomicUsize = AtomicUsize::new(0);
static COUNTER_SQL_ROW_ERRORS: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGINS_SEEN: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGINS_MISSING_STARS: AtomicUsize = AtomicUsize::new(0);
static COUNTER_ORIGINS_SHORT_HISTORY: AtomicUsize = AtomicUsize::new(0);
static COUNTER_NON_POSITIVE_SPAN: AtomicUsize = AtomicUsize::new(0);
static COUNTER_INVALID_RATE: AtomicUsize = AtomicUsize::new(0);
static COUNTER_RATES_COMPUTED: AtomicUsize = AtomicUsize::new(0);
static COUNTER_GROUP_0_1: AtomicUsize = AtomicUsize::new(0);
static COUNTER_GROUP_2_10: AtomicUsize = AtomicUsize::new(0);
static COUNTER_GROUP_11_100: AtomicUsize = AtomicUsize::new(0);
static COUNTER_GROUP_101_500: AtomicUsize = AtomicUsize::new(0);
static COUNTER_GROUP_500_PLUS: AtomicUsize = AtomicUsize::new(0);

fn format_counters() -> String {
    format!(
        "\n=== Defensive Counters ===\n\
         SQL rows read: {}\n\
         SQL row decode errors: {}\n\
         Origins seen in snapshot map: {}\n\
         Origins missing stars mapping: {}\n\
         Origins skipped (<2 snapshots): {}\n\
         Non-positive time spans corrected to 1 second: {}\n\
         Invalid frequency rates skipped: {}\n\
         Valid rates computed: {}\n\
         Group 0-1 count: {}\n\
         Group 2-10 count: {}\n\
         Group 11-100 count: {}\n\
         Group 101-500 count: {}\n\
         Group 500+ count: {}\n\
         ==========================\n",
        COUNTER_SQL_ROWS_READ.load(AtomicOrdering::Relaxed),
        COUNTER_SQL_ROW_ERRORS.load(AtomicOrdering::Relaxed),
        COUNTER_ORIGINS_SEEN.load(AtomicOrdering::Relaxed),
        COUNTER_ORIGINS_MISSING_STARS.load(AtomicOrdering::Relaxed),
        COUNTER_ORIGINS_SHORT_HISTORY.load(AtomicOrdering::Relaxed),
        COUNTER_NON_POSITIVE_SPAN.load(AtomicOrdering::Relaxed),
        COUNTER_INVALID_RATE.load(AtomicOrdering::Relaxed),
        COUNTER_RATES_COMPUTED.load(AtomicOrdering::Relaxed),
        COUNTER_GROUP_0_1.load(AtomicOrdering::Relaxed),
        COUNTER_GROUP_2_10.load(AtomicOrdering::Relaxed),
        COUNTER_GROUP_11_100.load(AtomicOrdering::Relaxed),
        COUNTER_GROUP_101_500.load(AtomicOrdering::Relaxed),
        COUNTER_GROUP_500_PLUS.load(AtomicOrdering::Relaxed),
    )
}

fn display_counters() {
    println!("{}", format_counters());
}

fn star_group(stars: f64) -> &'static str {
    if stars <= 1.0 {
        "0-1"
    } else if stars <= 10.0 {
        "2-10"
    } else if stars <= 100.0 {
        "11-100"
    } else if stars <= 500.0 {
        "101-500"
    } else {
        "500+"
    }
}

fn main() -> Result<()> {
    // Open SQLite mapping origin_url -> stars
    let db_path = "data/tags_alterations_full_2025-10_v2.db.bkp";
    let conn = Connection::open(db_path)?;
    println!("Loading stars from SQLite...");

    let estimated_star_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (SELECT DISTINCT origin_url FROM tags_with_stars WHERE stars IS NOT NULL)",
        [],
        |row| row.get(0),
    )?;
    let stars_pb = ProgressBar::new(estimated_star_rows.max(0) as u64);
    stars_pb.set_style(
        ProgressStyle::default_bar()
            .template("{msg} [{bar:40.green/blue}] {pos}/{len} rows {percent}% ({per_sec}, {eta})")?
            .progress_chars("#>-"),
    );
    stars_pb.set_message("Loading stars");
    
    let mut stmt = conn.prepare("SELECT DISTINCT origin_url, stars FROM tags_with_stars WHERE stars IS NOT NULL")?;
    let mut origin_to_stars = HashMap::new();
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
    })?;
    
    for row in rows {
        stars_pb.inc(1);
        match row {
            Ok((url, stars)) => {
                COUNTER_SQL_ROWS_READ.fetch_add(1, AtomicOrdering::Relaxed);
                origin_to_stars.insert(url, stars);
            }
            Err(_) => {
                COUNTER_SQL_ROW_ERRORS.fetch_add(1, AtomicOrdering::Relaxed);
            }
        }
    }
    stars_pb.finish_with_message("Stars loaded");
    println!("Loaded {} origins with stars.", origin_to_stars.len());

    let suffix = "full_2025-10_v2";
    println!("Extracting snapshots with snapshots_extraction...");
    let snapshots_by_origin = snapshots_extraction(suffix)?;
    let total_origins = snapshots_by_origin.len() as u64;

    const SECONDS_PER_YEAR: f64 = 365.25 * 24.0 * 60.0 * 60.0;
    
    let mut stats: HashMap<String, Vec<f64>> = HashMap::new();
    let origins_pb = ProgressBar::new(total_origins);
    origins_pb.set_style(
        ProgressStyle::default_bar()
            .template("{msg} [{bar:40.cyan/blue}] {pos}/{len} origins {percent}% ({per_sec}, {eta})")?
            .progress_chars("#>-"),
    );
    origins_pb.set_message("Computing per-origin rates");
    
    for (origin, snapshots) in snapshots_by_origin {
        COUNTER_ORIGINS_SEEN.fetch_add(1, AtomicOrdering::Relaxed);
        origins_pb.inc(1);

        let Some(&stars) = origin_to_stars.get(&origin) else {
            COUNTER_ORIGINS_MISSING_STARS.fetch_add(1, AtomicOrdering::Relaxed);
            continue;
        };

        if snapshots.len() < 2 {
            COUNTER_ORIGINS_SHORT_HISTORY.fetch_add(1, AtomicOrdering::Relaxed);
            continue;
        }

        let first_ts = snapshots.first().map(|s| s.date_seconds).unwrap_or(0);
        let last_ts = snapshots.last().map(|s| s.date_seconds).unwrap_or(0);
        let raw_span = last_ts - first_ts;
        if raw_span <= 0 {
            COUNTER_NON_POSITIVE_SPAN.fetch_add(1, AtomicOrdering::Relaxed);
        }
        let span_seconds = raw_span.max(1) as f64;
        // Use intervals, not points: N snapshots correspond to N-1 observation intervals.
        let freq_per_year = ((snapshots.len() - 1) as f64) * SECONDS_PER_YEAR / span_seconds;
        if !freq_per_year.is_finite() {
            COUNTER_INVALID_RATE.fetch_add(1, AtomicOrdering::Relaxed);
            continue;
        }

        let group = star_group(stars);
        match group {
            "0-1" => {
                COUNTER_GROUP_0_1.fetch_add(1, AtomicOrdering::Relaxed);
            }
            "2-10" => {
                COUNTER_GROUP_2_10.fetch_add(1, AtomicOrdering::Relaxed);
            }
            "11-100" => {
                COUNTER_GROUP_11_100.fetch_add(1, AtomicOrdering::Relaxed);
            }
            "101-500" => {
                COUNTER_GROUP_101_500.fetch_add(1, AtomicOrdering::Relaxed);
            }
            _ => {
                COUNTER_GROUP_500_PLUS.fetch_add(1, AtomicOrdering::Relaxed);
            }
        }

        stats
            .entry(group.to_string())
            .or_default()
            .push(freq_per_year);
        COUNTER_RATES_COMPUTED.fetch_add(1, AtomicOrdering::Relaxed);
    }
    origins_pb.finish_with_message("Per-origin rates computed");

    println!(
        "Skipped {} origins with fewer than 2 snapshots.",
        COUNTER_ORIGINS_SHORT_HISTORY.load(AtomicOrdering::Relaxed)
    );
    
    let mut results_output = String::new();
    results_output.push_str("\nSnapshot Frequency Results (snapshots/year)\n");
    results_output.push_str("==========================================\n");
    results_output.push_str(&format!(
        "Skipped {} origins with fewer than 2 snapshots.\n",
        COUNTER_ORIGINS_SHORT_HISTORY.load(AtomicOrdering::Relaxed)
    ));
    
    println!("Computing stats...");
    let groups = ["0-1", "2-10", "11-100", "101-500", "500+"];
    let stats_pb = ProgressBar::new(groups.len() as u64);
    stats_pb.set_style(
        ProgressStyle::default_bar()
            .template("{msg} [{bar:40.magenta/blue}] {pos}/{len} groups {percent}% ({per_sec}, {eta})")?
            .progress_chars("#>-"),
    );
    stats_pb.set_message("Aggregating quantiles");
    for group in groups {
        stats_pb.inc(1);
        if let Some(mut freqs) = stats.remove(group) {
            freqs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(CmpOrdering::Equal));
            let n = freqs.len();
            if n == 0 {
                println!("{}: No data", group);
                results_output.push_str(&format!("{}: No data\n", group));
                continue;
            }
            
            let q1 = freqs[n / 4];
            let median = freqs[n / 2];
            let q3 = freqs[(n * 3) / 4];
            let p90 = freqs[(n * 90) / 100];
            let p95 = freqs[(n * 95) / 100];
            let mean = freqs.iter().sum::<f64>() / n as f64;

            let line = format!(
                "Star Range: {:>7} | N: {:>8} | Mean: {:>8.2} | Q1: {:>8.2} | Median: {:>8.2} | Q3: {:>8.2} | P90: {:>8.2} | P95: {:>8.2} (snapshots/year)",
                group, n, mean, q1, median, q3, p90, p95
            );
            println!("{}", line);
            results_output.push_str(&line);
            results_output.push('\n');
        } else {
            println!("{}: No data", group);
            results_output.push_str(&format!("{}: No data\n", group));
        }
    }
    stats_pb.finish_with_message("Stats computed");

    display_counters();

    let mut log_file = File::create(format!(
        "data/counts/snapshots_frequency_stats_{}.log",
        suffix
    ))?;
    log_file.write_all(results_output.as_bytes())?;
    log_file.write_all(format_counters().as_bytes())?;
    
    Ok(())
}
