use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use num_format::{CustomFormat, Grouping, ToFormattedString};
use rayon::prelude::*;
use rusqlite::Connection;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        LazyLock,
        atomic::{AtomicUsize, Ordering},
    },
};
use swh_graph::mph::DynMphf;
use swh_graph::{NodeType, graph::*, labels::EdgeLabel};
use tags_alterations::lib_tmp::SnapshotInfo;

static NO_INITIALISATION: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
#[allow(dead_code)]
struct TagAlteration {
    origin_url: String,
    tag_name: String,
    type_: String,
    old_snapshot: String,
    old_snap_timestamp: i64,
    old_revision: String,
    old_root_dir: Option<String>,
    new_snapshot: String,
    new_snap_timestamp: i64,
}

#[derive(Debug)]
struct DeletionCreationResult {
    origin_url: String,
    tag_name: String,
    type_: String,
    old_snapshot: String,
    old_snap_timestamp: i64,
    old_revision: String,
    old_root_dir: Option<String>,
    new_snapshot: String,
    new_snap_timestamp: i64,
    creation_type: String,
    creation_rev: Option<String>,
    creation_rev_ts: Option<i64>,
    creation_root_dir: Option<String>,
    creation_snapshot: String,
    creation_snap_ts: i64,
    creation_delta: i64,
}

static FORMAT: LazyLock<CustomFormat> = LazyLock::new(|| {
    CustomFormat::builder()
        .grouping(Grouping::Standard)
        .separator("_")
        .build()
        .unwrap()
});

pub fn deletion_creation<G: SwhFullGraph + Sync>(
    graph: &G,
    conn: &Connection,
    snapshots: HashMap<String, Vec<SnapshotInfo>>,
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
        "SELECT origin_url, tag_name, type, old_snapshot, old_snap_timestamp, old_revision, old_root_dir, new_snapshot, new_snap_timestamp FROM tag_inconsistencies WHERE new_revision IS NULL",
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
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    println!(
        "Retrieved {} deletions",
        rows.len().to_formatted_string(&*FORMAT)
    );

    conn.execute(
        "CREATE TABLE IF NOT EXISTS deletion_creation_v2 (
            origin_url TEXT NOT NULL,
            tag_name TEXT NOT NULL,
            type TEXT NOT NULL,
            old_snapshot TEXT NOT NULL,
            old_snap_timestamp INTEGER NOT NULL,
            old_revision TEXT NOT NULL,
            old_root_dir TEXT,
            new_snapshot TEXT NOT NULL,
            new_snap_timestamp INTEGER NOT NULL,
            creation_type TEXT NOT NULL,
            creation_rev TEXT,
            creation_rev_ts INTEGER,
            creation_root_dir TEXT,
            creation_snapshot TEXT NOT NULL,
            creation_snap_ts INTEGER NOT NULL,
            creation_delta INTEGER NOT NULL
        )",
        [],
    )?;

    let pb = ProgressBar::new(rows.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("██░"),
    );

    let results: Vec<DeletionCreationResult> = rows
        .into_par_iter()
        .filter_map(|tag_alteration| {
            pb.inc(1);
            if let Some(creation) = analyse_deletion(&tag_alteration, graph, &snapshots) {
                Some(DeletionCreationResult {
                    origin_url: tag_alteration.origin_url,
                    tag_name: tag_alteration.tag_name,
                    type_: tag_alteration.type_,
                    old_snapshot: tag_alteration.old_snapshot,
                    old_snap_timestamp: tag_alteration.old_snap_timestamp,
                    old_revision: tag_alteration.old_revision,
                    old_root_dir: tag_alteration.old_root_dir,
                    new_snapshot: tag_alteration.new_snapshot,
                    new_snap_timestamp: tag_alteration.new_snap_timestamp,
                    creation_delta: creation.delta,
                    creation_rev: creation.rev,
                    creation_rev_ts: creation.rev_ts,
                    creation_root_dir: creation.root_dir,
                    creation_snap_ts: creation.snap_ts,
                    creation_snapshot: creation.snapshot,
                    creation_type: creation.type_,
                })
            } else {
                None
            }
        })
        .collect();

    pb.finish();

    println!(
        "Found {} deletion->creation cases",
        results.len().to_formatted_string(&*FORMAT)
    );

    if !results.is_empty() {
        let mut stmt = conn.prepare(
            "INSERT INTO deletion_creation_v2 (
                origin_url, tag_name, type, old_snapshot, old_snap_timestamp,
                old_revision, old_root_dir, new_snapshot, new_snap_timestamp, creation_type, creation_rev, creation_rev_ts, creation_root_dir, creation_snapshot, creation_snap_ts, creation_delta
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        )?;

        for result in &results {
            stmt.execute((
                &result.origin_url,
                &result.tag_name,
                &result.type_,
                &result.old_snapshot,
                result.old_snap_timestamp,
                &result.old_revision,
                &result.old_root_dir,
                &result.new_snapshot,
                result.new_snap_timestamp,
                &result.creation_type,
                &result.creation_rev,
                result.creation_rev_ts,
                &result.creation_root_dir,
                &result.creation_snapshot,
                result.creation_snap_ts,
                result.creation_delta
            ))?;
        }

        println!(
            "Inserted {} rows into deletion_creation table",
            results.len().to_formatted_string(&*FORMAT)
        );
    }

    Ok(())
}

fn analyse_deletion<G: SwhFullGraph>(
    tag_alteration: &TagAlteration,
    graph: &G,
    snapshots: &HashMap<String, Vec<SnapshotInfo>>,
) -> Option<Creation> {
    let Some(snapshots) = snapshots.get(&tag_alteration.origin_url) else {
        return None;
    };
    let mut annotated_tags: HashSet<String> = HashSet::new();
    let mut lightweight_tags: HashSet<String> = HashSet::new();
    let mut previous_snap: Option<&SnapshotInfo> = None;
    let mut cpt_diff = 0;
    let mut delta = tag_alteration.new_snap_timestamp;
    for snapshot in snapshots {
        if snapshot.date_seconds < tag_alteration.new_snap_timestamp {
            previous_snap = Some(snapshot);
            continue;
        }
        if snapshot.date_seconds == tag_alteration.new_snap_timestamp {
            if let Some(prev_snap) = previous_snap {
                if let Some(ref swhid) = prev_snap.snapshot {
                    initiate_tags(&mut annotated_tags, &mut lightweight_tags, graph, swhid);
                    if annotated_tags.is_empty() && lightweight_tags.is_empty() {
                        NO_INITIALISATION.fetch_add(1, Ordering::Relaxed);
                        return None;
                    }
                }
            }
        }
        if snapshot.date_seconds > tag_alteration.new_snap_timestamp {
            cpt_diff += 1;
            if cpt_diff > 1 {
                delta = match previous_snap {
                    None => 0,
                    Some(snap) => snap.date_seconds - tag_alteration.new_snap_timestamp,
                };
            }
        }
        if let Some(_) = snapshot.snapshot {
            if let Some(creation) = spot_deletion_creation(
                &tag_alteration,
                &annotated_tags,
                &lightweight_tags,
                delta,
                graph,
                snapshot,
            ) {
                return Some(creation);
            }
        }
        previous_snap = Some(snapshot);
    }
    None
}

fn initiate_tags<G: SwhFullGraph>(
    annotated_tags: &mut HashSet<String>,
    lightweight_tags: &mut HashSet<String>,
    graph: &G,
    snapshot: &str,
) {
    let node = match graph.properties().node_id(snapshot) {
        Err(_) => {
            let Ok(swhid) = graph
                .properties()
                .node_id(format!("swh:1:snp:{}", snapshot).as_str())
            else {
                return;
            };
            swhid
        }
        Ok(swhid) => swhid,
    };
    for (succ, labels) in graph.labeled_successors(node) {
        for label in labels {
            if let EdgeLabel::Branch(branch) = label {
                let Ok(branch_name) =
                    String::from_utf8(graph.properties().label_name(branch.label_name_id()))
                else {
                    continue;
                };
                if branch_name.contains("/tags/") {
                    match graph.properties().node_type(succ) {
                        NodeType::Revision => {
                            lightweight_tags.insert(branch_name);
                        }
                        NodeType::Release => {
                            annotated_tags.insert(branch_name);
                        }
                        _ => (),
                    }
                }
            }
        }
    }
}

struct Creation {
    type_: String,
    rev: Option<String>,
    rev_ts: Option<i64>,
    root_dir: Option<String>,
    snapshot: String,
    snap_ts: i64,
    delta: i64,
}

impl Creation {
    fn new<G: SwhFullGraph>(
        snapshot: &SnapshotInfo,
        delta: i64,
        release: usize,
        type_: &str,
        graph: &G,
    ) -> Self {
        let rev = match type_ {
            "lightweight" => Some(graph.properties().swhid(release).to_string()),
            "annotated" => {
                let mut swhid = None;
                for succ in graph.successors(release) {
                    swhid = match graph.properties().node_type(succ) {
                        NodeType::Revision => Some(graph.properties().swhid(succ).to_string()),
                        NodeType::Release => {
                            match graph.successors(succ).into_iter().find(|succ_bis| {
                                graph.properties().node_type(*succ_bis) == NodeType::Revision
                            }) {
                                None => None,
                                Some(node) => Some(graph.properties().swhid(node).to_string()),
                            }
                        }
                        _ => None,
                    };
                }
                swhid
            }
            _ => None,
        };
        let rev_ts = match rev.as_ref() {
            None => None,
            Some(rev) => {
                let node = graph.properties().node_id(rev.as_str()).unwrap();
                graph.properties().committer_timestamp(node)
            }
        };
        let root_dir = match rev.as_ref() {
            None => None,
            Some(rev) => {
                let node = graph.properties().node_id(rev.as_str()).unwrap();
                match swh_graph_stdlib::find_root_dir(graph, node) {
                    Err(_) => None,
                    Ok(root_dir) => root_dir,
                }
            }
        };
        Creation {
            type_: type_.to_string(),
            rev,
            rev_ts,
            root_dir: root_dir.map(|rd| graph.properties().swhid(rd).to_string()),
            snapshot: snapshot.snapshot.clone().unwrap(),
            snap_ts: snapshot.date_seconds,
            delta,
        }
    }
}

fn spot_deletion_creation<G: SwhFullGraph>(
    tag_alteration: &TagAlteration,
    annotated_tags: &HashSet<String>,
    lightweight_tags: &HashSet<String>,
    delta: i64,
    graph: &G,
    snapshot: &SnapshotInfo,
) -> Option<Creation> {
    let snap_node = match graph
        .properties()
        .node_id(snapshot.snapshot.as_ref().unwrap().as_str())
    {
        Err(_) => {
            let Ok(swhid) = graph
                .properties()
                .node_id(format!("swh:1:snp:{}", snapshot.snapshot.as_ref().unwrap()).as_str())
            else {
                return None;
            };
            swhid
        }
        Ok(swhid) => swhid,
    };
    for (succ, labels) in graph.labeled_successors(snap_node) {
        for label in labels {
            if let EdgeLabel::Branch(branch) = label {
                let Ok(branch_name) =
                    String::from_utf8(graph.properties().label_name(branch.label_name_id()))
                else {
                    continue;
                };
                if !branch_name.contains("/tags/") {
                    continue;
                }
                match graph.properties().node_type(succ) {
                    NodeType::Release => {
                        if annotated_tags.contains(&branch_name) {
                            continue;
                        }
                        if branch_name == tag_alteration.tag_name {
                            return Some(Creation::new(snapshot, delta, succ, "annotated", graph))
                        }
                    }
                    NodeType::Revision => {
                        if lightweight_tags.contains(&branch_name) {
                            continue;
                        }
                        if branch_name == tag_alteration.tag_name {
                            return Some(Creation::new(snapshot, delta, succ, "lightweight", graph))
                        }
                    }
                    _ => (),
                }
            }
        }
    }
    None
}

pub fn main() -> Result<()> {
    let graph = SwhBidirectionalGraph::new("/dev/shm/swh-graph/current/graph")?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;
    let conn = Connection::open(format!("data/tags_alterations_full_2025-10_v2.db"))?;

    let snapshots = tags_alterations::lib_tmp::snapshots_extraction("full_2025-10_v2")?;

    deletion_creation(&graph, &conn, snapshots)?;

    println!(
        "initialisation error: {}",
        NO_INITIALISATION.load(Ordering::Relaxed)
    );
    drop(conn);
    Ok(())
}
