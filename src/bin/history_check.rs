use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use num_format::{CustomFormat, Grouping, ToFormattedString};
use rayon::prelude::*;
use rusqlite::{Connection, params};
use std::collections::HashSet;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use swh_graph::mph::DynMphf;
use swh_graph::{NodeType, graph::*};

static FORMAT: LazyLock<CustomFormat> = LazyLock::new(|| {
    CustomFormat::builder()
        .grouping(Grouping::Standard)
        .separator("_")
        .build()
        .unwrap()
});

// Static counters for defensive programming
static COUNTER_TOTAL_ALTERATIONS: AtomicUsize = AtomicUsize::new(0);
static COUNTER_MOVES: AtomicUsize = AtomicUsize::new(0);
static COUNTER_DELETIONS_WITH_CREATION: AtomicUsize = AtomicUsize::new(0);
static COUNTER_DELETIONS_WITHOUT_CREATION: AtomicUsize = AtomicUsize::new(0);
static COUNTER_OLD_REV_NOT_FOUND: AtomicUsize = AtomicUsize::new(0);
static COUNTER_NEW_REV_NOT_FOUND: AtomicUsize = AtomicUsize::new(0);
static COUNTER_CREATION_REV_NOT_FOUND: AtomicUsize = AtomicUsize::new(0);
static COUNTER_SNAPSHOT_NOT_FOUND: AtomicUsize = AtomicUsize::new(0);
static COUNTER_IN_HISTORY: AtomicUsize = AtomicUsize::new(0);
static COUNTER_NOT_IN_HISTORY: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
#[allow(dead_code)]
struct TagAlteration {
    origin_url: String,
    tag_name: String,
    type_: String,
    old_snapshot: String,
    old_snap_timestamp: i64,
    old_revision: Option<String>,
    old_root_dir: Option<String>,
    new_snapshot: String,
    new_snap_timestamp: i64,
    new_revision: Option<String>,
    creation_rev: Option<String>,
}

struct HistoryCheckResult {
    origin_url: String,
    tag_name: String,
    type_: String,
    old_snapshot: String,
    old_snap_timestamp: i64,
    old_revision: Option<String>,
    old_root_dir: Option<String>,
    new_snapshot: String,
    new_snap_timestamp: i64,
    check_type: String,
    is_in_history: bool,
}

fn main() -> Result<()> {
    println!("Loading graph...");
    let graph = SwhBidirectionalGraph::new("/dev/shm/swh-graph/current/graph")?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;

    let start = Instant::now();
    println!("Querying database...");
    let mut conn = Connection::open(format!("../data/tags_alterations_full_2025-10_v2.db"))?;

    let table_exists = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='tag_inconsistencies'")
        .and_then(|mut stmt| stmt.exists([]))
        .unwrap_or(false);

    if !table_exists {
        println!("Table 'tag_inconsistencies' doesn't exist");
        return Ok(());
    }

    // Load deletion_creation_v2 data for deletions
    let mut creation_stmt = conn.prepare(
        "SELECT origin_url, tag_name, type, old_snapshot, old_snap_timestamp, creation_rev 
         FROM deletion_creation_v2",
    )?;

    let creation_data: Vec<(String, String, String, String, i64, Option<String>)> = creation_stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    drop(creation_stmt);

    // Create a lookup map for creation data
    let mut creation_map = std::collections::HashMap::new();
    for (origin, tag, type_, old_snap, old_ts, creation_rev) in creation_data {
        creation_map.insert((origin, tag, type_, old_snap, old_ts), creation_rev);
    }

    // Query all tag alterations
    let mut stmt = conn.prepare(
        "SELECT origin_url, tag_name, type, old_snapshot, old_snap_timestamp, 
                old_revision, old_root_dir, new_snapshot, new_snap_timestamp, new_revision
         FROM tag_inconsistencies",
    )?;

    let rows: Vec<TagAlteration> = stmt
        .query_map([], |row| {
            Ok(TagAlteration {
                origin_url: row.get(0)?,
                tag_name: row.get(1)?,
                type_: row.get(2)?,
                old_snapshot: row.get(3)?,
                old_snap_timestamp: row.get(4)?,
                old_revision: row.get(5)?,
                old_root_dir: row.get(6)?,
                new_snapshot: row.get(7)?,
                new_snap_timestamp: row.get(8)?,
                new_revision: row.get(9)?,
                creation_rev: None, // Will be filled from creation_map
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    drop(stmt);

    // Add creation_rev from the map
    let rows: Vec<TagAlteration> = rows
        .into_iter()
        .map(|mut row| {
            let key = (
                row.origin_url.clone(),
                row.tag_name.clone(),
                row.type_.clone(),
                row.old_snapshot.clone(),
                row.old_snap_timestamp,
            );
            row.creation_rev = creation_map.get(&key).cloned().flatten();
            row
        })
        .collect();

    println!("Done retrieving in: {:?}", start.elapsed());
    println!(
        "Retrieved {} tag alterations",
        rows.len().to_formatted_string(&*FORMAT)
    );

    // Create result table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS history_check_results (
            origin_url TEXT NOT NULL,
            tag_name TEXT NOT NULL,
            type TEXT NOT NULL,
            old_snapshot TEXT NOT NULL,
            old_snap_timestamp INTEGER NOT NULL,
            old_revision TEXT,
            old_root_dir TEXT,
            new_snapshot TEXT NOT NULL,
            new_snap_timestamp INTEGER NOT NULL,
            check_type TEXT NOT NULL,
            is_in_history INTEGER NOT NULL,
            PRIMARY KEY(origin_url, tag_name, type, old_snapshot, old_snap_timestamp, old_revision, old_root_dir, new_snapshot, new_snap_timestamp)
        )",
        [],
    )?;

    // Clear existing results
    conn.execute("DELETE FROM history_check_results", [])?;

    let pb = ProgressBar::new(rows.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {percent}% ({eta})")
            .unwrap()
            .progress_chars("██░"),
    );

    println!("Starting history checks...");

    let results: Vec<HistoryCheckResult> = rows
        .into_par_iter()
        .map(|alteration| {
            pb.inc(1);
            let (check_type, is_in_history) = check_history(&alteration, &graph);
            HistoryCheckResult {
                origin_url: alteration.origin_url.clone(),
                tag_name: alteration.tag_name.clone(),
                type_: alteration.type_.clone(),
                old_snapshot: alteration.old_snapshot.clone(),
                old_snap_timestamp: alteration.old_snap_timestamp,
                old_revision: alteration.old_revision.clone(),
                old_root_dir: alteration.old_root_dir.clone(),
                new_snapshot: alteration.new_snapshot.clone(),
                new_snap_timestamp: alteration.new_snap_timestamp,
                check_type,
                is_in_history,
            }
        })
        .collect();

    pb.finish_with_message("History checks complete");

    // Store results
    println!("Storing results in database...");
    let tx = conn.transaction()?;
    {
        let mut insert_stmt = tx.prepare(
            "INSERT INTO history_check_results 
             (origin_url, tag_name, type, old_snapshot, old_snap_timestamp, old_revision, old_root_dir, new_snapshot, new_snap_timestamp, check_type, is_in_history) 
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )?;

        for result in &results {
            insert_stmt.execute(params![
                result.origin_url,
                result.tag_name,
                result.type_,
                result.old_snapshot,
                result.old_snap_timestamp,
                result.old_revision,
                result.old_root_dir,
                result.new_snapshot,
                result.new_snap_timestamp,
                result.check_type,
                result.is_in_history as i32
            ])?;
        }
    }
    tx.commit()?;

    display_counters();

    println!("\nTotal time: {:?}", start.elapsed());
    println!("Results stored in 'history_check_results' table");

    Ok(())
}

fn check_history<G: SwhFullGraph>(alteration: &TagAlteration, graph: &G) -> (String, bool) {
    COUNTER_TOTAL_ALTERATIONS.fetch_add(1, Ordering::Relaxed);

    // Get old_revision node
    let Some(old_rev_swhid) = &alteration.old_revision else {
        // No old revision to check
        return ("no_old_revision".to_string(), false);
    };

    let old_rev_swhid_full = if old_rev_swhid.starts_with("swh:") {
        old_rev_swhid.clone()
    } else {
        format!("swh:1:rev:{}", old_rev_swhid)
    };

    let old_rev_node = match graph.properties().node_id(old_rev_swhid_full.as_str()) {
        Ok(node) => node,
        Err(_) => {
            COUNTER_OLD_REV_NOT_FOUND.fetch_add(1, Ordering::Relaxed);
            return ("old_revision_not_found".to_string(), false);
        }
    };

    // Determine which revision(s) to check against
    if let Some(new_rev_swhid) = &alteration.new_revision {
        // Case 1: Move - check if old_revision is in history of new_revision
        COUNTER_MOVES.fetch_add(1, Ordering::Relaxed);

        let new_rev_swhid_full = if new_rev_swhid.starts_with("swh:") {
            new_rev_swhid.clone()
        } else {
            format!("swh:1:rev:{}", new_rev_swhid)
        };

        let new_rev_node = match graph.properties().node_id(new_rev_swhid_full.as_str()) {
            Ok(node) => node,
            Err(_) => {
                COUNTER_NEW_REV_NOT_FOUND.fetch_add(1, Ordering::Relaxed);
                return ("new_revision_not_found".to_string(), false);
            }
        };

        (
            "direct_history_vs_new_revision".to_string(),
            is_in_history(old_rev_node, new_rev_node, graph),
        )
    } else if let Some(creation_rev_swhid) = &alteration.creation_rev {
        // Case 2: Deletion with creation_rev - check if old_revision is in history of creation_rev
        COUNTER_DELETIONS_WITH_CREATION.fetch_add(1, Ordering::Relaxed);

        let creation_rev_swhid_full = if creation_rev_swhid.starts_with("swh:") {
            creation_rev_swhid.clone()
        } else {
            format!("swh:1:rev:{}", creation_rev_swhid)
        };

        let creation_rev_node = match graph.properties().node_id(creation_rev_swhid_full.as_str()) {
            Ok(node) => node,
            Err(_) => {
                COUNTER_CREATION_REV_NOT_FOUND.fetch_add(1, Ordering::Relaxed);
                return ("creation_revision_not_found".to_string(), false);
            }
        };

        (
            "direct_history_vs_creation_revision".to_string(),
            is_in_history(old_rev_node, creation_rev_node, graph),
        )
    } else {
        // Case 3: Deletion without creation_rev - check if old_revision is in history of any revision in new_snapshot
        COUNTER_DELETIONS_WITHOUT_CREATION.fetch_add(1, Ordering::Relaxed);

        let new_snap_swhid = if alteration.new_snapshot.starts_with("swh:") {
            alteration.new_snapshot.clone()
        } else {
            format!("swh:1:snp:{}", alteration.new_snapshot)
        };

        let new_snap_node = match graph.properties().node_id(new_snap_swhid.as_str()) {
            Ok(node) => node,
            Err(_) => {
                COUNTER_SNAPSHOT_NOT_FOUND.fetch_add(1, Ordering::Relaxed);
                return ("snapshot_not_found".to_string(), false);
            }
        };

        (
            "any_branch_history_in_snapshot".to_string(),
            is_in_any_branch_history(old_rev_node, new_snap_node, graph),
        )
    }
}

/// Check if target_rev is reachable from start_rev by following parent revisions
fn is_in_history<G: SwhFullGraph>(target_rev: usize, start_rev: usize, graph: &G) -> bool {
    if target_rev == start_rev {
        COUNTER_IN_HISTORY.fetch_add(1, Ordering::Relaxed);
        return true;
    }

    let mut visited = HashSet::new();
    let mut to_visit = Vec::new();
    to_visit.push(start_rev);

    while let Some(node) = to_visit.pop() {
        if !visited.insert(node) {
            continue;
        }

        if node == target_rev {
            COUNTER_IN_HISTORY.fetch_add(1, Ordering::Relaxed);
            return true;
        }

        let node_type = graph.properties().node_type(node);

        // Only traverse through revisions (commits)
        if node_type == NodeType::Revision {
            for successor in graph.successors(node) {
                let succ_type = graph.properties().node_type(successor);
                if succ_type == NodeType::Revision {
                    to_visit.push(successor);
                }
            }
        }
    }

    COUNTER_NOT_IN_HISTORY.fetch_add(1, Ordering::Relaxed);
    false
}

/// Check if target_rev is reachable from any revision in the snapshot
fn is_in_any_branch_history<G: SwhFullGraph>(
    target_rev: usize,
    snapshot: usize,
    graph: &G,
) -> bool {
    // First, collect all head revisions from the snapshot
    let mut head_revisions = Vec::new();

    for (succ, labels) in graph.labeled_successors(snapshot) {
        for _label in labels {
            let succ_type = graph.properties().node_type(succ);
            match succ_type {
                NodeType::Revision => {
                    head_revisions.push(succ);
                }
                NodeType::Release => {
                    // Follow release to get the revision
                    for rel_succ in graph.successors(succ) {
                        if graph.properties().node_type(rel_succ) == NodeType::Revision {
                            head_revisions.push(rel_succ);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Check if target_rev is in the history of any head revision
    for head_rev in head_revisions {
        if is_in_history(target_rev, head_rev, graph) {
            return true;
        }
    }

    COUNTER_NOT_IN_HISTORY.fetch_add(1, Ordering::Relaxed);
    false
}

fn display_counters() {
    println!("{}", format_counters());
}

fn format_counters() -> String {
    format!(
        "\n=== History Check Counters ===\n\
         Total alterations processed: {}\n\
         Moves (new_revision exists): {}\n\
         Deletions with creation_rev: {}\n\
         Deletions without creation_rev: {}\n\
         \n\
         Errors:\n\
         Old revision not found in graph: {}\n\
         New revision not found in graph: {}\n\
         Creation revision not found in graph: {}\n\
         Snapshot not found in graph: {}\n\
         \n\
         Results:\n\
         Old revision IS in history: {}\n\
         Old revision NOT in history: {}\n\
         ======================================\n",
        COUNTER_TOTAL_ALTERATIONS.load(Ordering::Relaxed),
        COUNTER_MOVES.load(Ordering::Relaxed),
        COUNTER_DELETIONS_WITH_CREATION.load(Ordering::Relaxed),
        COUNTER_DELETIONS_WITHOUT_CREATION.load(Ordering::Relaxed),
        COUNTER_OLD_REV_NOT_FOUND.load(Ordering::Relaxed),
        COUNTER_NEW_REV_NOT_FOUND.load(Ordering::Relaxed),
        COUNTER_CREATION_REV_NOT_FOUND.load(Ordering::Relaxed),
        COUNTER_SNAPSHOT_NOT_FOUND.load(Ordering::Relaxed),
        COUNTER_IN_HISTORY.load(Ordering::Relaxed),
        COUNTER_NOT_IN_HISTORY.load(Ordering::Relaxed),
    )
}
