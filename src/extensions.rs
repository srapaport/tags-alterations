use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use num_format::{CustomFormat, Grouping, ToFormattedString};
use rayon::prelude::*;
use rusqlite::Connection;
use std::sync::{
    LazyLock,
    atomic::{AtomicUsize, Ordering},
};
use swh_graph::{NodeType, graph::*, labels::EdgeLabel};

static COUNTER_INVALID_UTF8_BRANCH_NAME: AtomicUsize = AtomicUsize::new(0);
static NOT_FOUND: AtomicUsize = AtomicUsize::new(0);
static NONE_TS: AtomicUsize = AtomicUsize::new(0);
static INCONSIS_TS: AtomicUsize = AtomicUsize::new(0);
static NO_INCONSIS: AtomicUsize = AtomicUsize::new(0);
static BOTH_NONE: AtomicUsize = AtomicUsize::new(0);
static EQUALS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
#[allow(dead_code)]
struct TagAlteration {
    id: i64,
    origin_url: String,
    tag_name: String,
    type_: String,
    old_snapshot: String,
    old_snapshot_cpt: i64,
    old_snap_timestamp: i64,
    old_revision: String,
    old_rev_timestamp: i64,
    old_root_dir: Option<String>,
    new_snapshot: String,
    new_snapshot_cpt: i64,
    new_snap_timestamp: i64,
    new_revision: Option<String>,
    new_rev_timestamp: Option<i64>,
    new_root_dir: Option<String>,
    min_delta: i64,
}

static FORMAT: LazyLock<CustomFormat> = LazyLock::new(|| {
    CustomFormat::builder()
        .grouping(Grouping::Standard)
        .separator("_")
        .build()
        .unwrap()
});

pub fn release_timestamp_anomalies<G: SwhFullGraph + Sync>(
    graph: &G,
    conn: &Connection,
) -> Result<()> {
    let table_exists = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='tag_inconsistencies'")
        .and_then(|mut stmt| stmt.exists([]))
        .unwrap_or(false);
    if !table_exists {
        println!("Table doesn't exist");
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT * FROM tag_inconsistencies WHERE type = 'annotated' AND new_revision IS NOT NULL",
    )?;
    let rows: Vec<TagAlteration> = stmt
        .query_map([], |row| {
            Ok(TagAlteration {
                id: row.get(0)?,
                origin_url: row.get(1)?,
                tag_name: row.get(2)?,
                type_: row.get(3)?,
                old_snapshot: row.get(4)?,
                old_snapshot_cpt: row.get(5)?,
                old_snap_timestamp: row.get(6)?,
                old_revision: row.get(7)?,
                old_rev_timestamp: row.get(8)?,
                old_root_dir: row.get(9)?,
                new_snapshot: row.get(10)?,
                new_snapshot_cpt: row.get(11)?,
                new_snap_timestamp: row.get(12)?,
                new_revision: row.get(13)?,
                new_rev_timestamp: row.get(14)?,
                new_root_dir: row.get(15)?,
                min_delta: row.get(16)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    println!(
        "Retrieved {} annotated moves",
        rows.len().to_formatted_string(&*FORMAT)
    );

    let pb = ProgressBar::new(rows.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("██░"),
    );

    rows.into_par_iter().for_each(|tag_alteration| {
        let old_snapshot = graph
            .properties()
            .node_id(tag_alteration.old_snapshot.as_str())
            .ok()
            .unwrap();
        let old_timestamp = get_release_ts(graph, old_snapshot, &tag_alteration.tag_name).unwrap();
        let new_snapshot = graph
            .properties()
            .node_id(tag_alteration.new_snapshot.as_str())
            .ok()
            .unwrap();
        let new_timestamp = get_release_ts(graph, new_snapshot, &tag_alteration.tag_name).unwrap();
        if old_timestamp.0.is_none() && new_timestamp.0.is_none(){
            BOTH_NONE.fetch_add(1, Ordering::Relaxed);
        }
        else if old_timestamp.0 != new_timestamp.0 {
            if old_timestamp.0.is_none() || new_timestamp.0.is_none() {
                NONE_TS.fetch_add(1, Ordering::Relaxed);
            } else if old_timestamp.0.unwrap() > new_timestamp.0.unwrap() {
                INCONSIS_TS.fetch_add(1, Ordering::Relaxed);
                if INCONSIS_TS.load(Ordering::Relaxed) < 5{
                    println!("Old: {} ts: {:?}", old_timestamp.1, old_timestamp.0);
                    println!("New: {} ts: {:?}", new_timestamp.1, new_timestamp.0);
                }
            } else {
                NO_INCONSIS.fetch_add(1, Ordering::Relaxed);
            }
        }
        else{
            EQUALS.fetch_add(1, Ordering::Relaxed);
        }
        pb.inc(1);
    });
    pb.finish();
    display_counters();
    Ok(())
}

fn display_counters() {
    println!("\n=== Counters ===");
    println!(
        "Invalid UTF8 branch names: {}",
        COUNTER_INVALID_UTF8_BRANCH_NAME
            .load(Ordering::Relaxed)
            .to_formatted_string(&*FORMAT)
    );
    println!(
        "Tags not found: {}",
        NOT_FOUND
            .load(Ordering::Relaxed)
            .to_formatted_string(&*FORMAT)
    );
    println!(
        "None timestamp: {}",
        NONE_TS
            .load(Ordering::Relaxed)
            .to_formatted_string(&*FORMAT)
    );
    println!(
        "Inconsistent timestamps: {}",
        INCONSIS_TS
            .load(Ordering::Relaxed)
            .to_formatted_string(&*FORMAT)
    );
    println!(
        "No inconsistencies: {}",
        NO_INCONSIS
            .load(Ordering::Relaxed)
            .to_formatted_string(&*FORMAT)
    );
    println!(
        "Equals: {}",
        EQUALS
            .load(Ordering::Relaxed)
            .to_formatted_string(&*FORMAT)
    );
    println!(
        "Both None: {}",
        BOTH_NONE
            .load(Ordering::Relaxed)
            .to_formatted_string(&*FORMAT)
    );
}

fn get_release_ts<G: SwhFullGraph>(graph: &G, snapshot: usize, tag_name: &str) -> Option<(Option<i64>, String)> {
    for (succ, labels) in graph.labeled_successors(snapshot) {
        if graph.properties().node_type(succ) != NodeType::Release {
            continue;
        }
        for label in labels {
            if let EdgeLabel::Branch(branch) = label {
                let Ok(branch_name) =
                    String::from_utf8(graph.properties().label_name(branch.label_name_id()))
                else {
                    COUNTER_INVALID_UTF8_BRANCH_NAME.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                if branch_name.as_str() == tag_name {
                    let rel_swhid = graph.properties().swhid(succ).to_string();
                    return Some((graph.properties().author_timestamp(succ), rel_swhid));
                }
            }
        }
        for (succ_bis, labels) in graph.labeled_successors(succ) {
            if graph.properties().node_type(succ_bis) != NodeType::Release {
                continue;
            }
            for label in labels {
                if let EdgeLabel::Branch(branch) = label {
                    let Ok(branch_name) =
                        String::from_utf8(graph.properties().label_name(branch.label_name_id()))
                    else {
                        COUNTER_INVALID_UTF8_BRANCH_NAME.fetch_add(1, Ordering::Relaxed);
                        continue;
                    };
                    if branch_name.as_str() == tag_name {
                        let rel_swhid = graph.properties().swhid(succ_bis).to_string();
                        return Some((graph.properties().author_timestamp(succ_bis), rel_swhid));
                    }
                }
            }
        }
    }
    NOT_FOUND.fetch_add(1, Ordering::Relaxed);
    return None;
}
