use std::sync::LazyLock;
use std::env;

use anyhow::Result;
use chrono::NaiveDateTime;
use indicatif::{ProgressBar, ProgressStyle};
use num_format::{CustomFormat, Grouping, ToFormattedString};
use rusqlite::Connection;
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Instant;
use swh_graph::{NodeType, graph::*, labels::EdgeLabel, mph::DynMphf};
use swh_graph_stdlib::diff;

use rayon::prelude::*;

struct Classified {
    diff_rel: Option<DiffRelease>,
    diff_rev: Option<DiffRevision>,
    diff_dir: Option<DiffDirectory>,
    old_release_swhid: Option<String>,
    new_release_swhid: Option<String>,
    old_revision_swhid: Option<String>,
    new_revision_swhid: Option<String>,
    old_directory_swhid: Option<String>,
    new_directory_swhid: Option<String>,
}

struct DiffRelease {
    message_differs: bool,
    author_differs: bool,
    author_timestamp_differs: bool,
}

struct DiffRevision {
    message_differs: bool,
    author_differs: bool,
    committer_differs: bool,
    author_timestamp_differs: bool,
    committer_timestamp_differs: bool,
}

struct DiffDirectory {
    added: usize,
    deleted: usize,
    modified: usize,
    renamed: usize,
    added_files: Vec<String>,
    deleted_files: Vec<String>,
    modified_files: Vec<String>,
}

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
    category: String,
    status: Option<String>,
    creation_type: Option<String>,
    creation_snapshot: Option<String>,
    new_revision: Option<String>,
    new_root_dir: Option<String>,
    creation_rev: Option<String>,
    creation_root_dir: Option<String>,
}

static FORMAT: LazyLock<CustomFormat> = LazyLock::new(|| {
    CustomFormat::builder()
        .grouping(Grouping::Standard)
        .separator("_")
        .build()
        .unwrap()
});

static NO_TYPES: AtomicUsize = AtomicUsize::new(0);
static NO_RELS: AtomicUsize = AtomicUsize::new(0);
static TYPES: AtomicUsize = AtomicUsize::new(0);
static RELS: AtomicUsize = AtomicUsize::new(0);
static DIR_NODE_ID_ERR: AtomicUsize = AtomicUsize::new(0);
static DIR_NAME_UTF8_ERR: AtomicUsize = AtomicUsize::new(0);
static TREE_DIFF_ERR: AtomicUsize = AtomicUsize::new(0);
static REV_NODE_ID_ERR: AtomicUsize = AtomicUsize::new(0);
static ANNOTATED_WITHOUT_REL: AtomicUsize = AtomicUsize::new(0);
static REL_MESSAGE_ERR: AtomicUsize = AtomicUsize::new(0);
static REL_AUTHOR_ERR: AtomicUsize = AtomicUsize::new(0);
static REL_AUTHOR_TIMESTAMP_ERR: AtomicUsize = AtomicUsize::new(0);

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let get_arg = |flag: &str, env_key: &str| -> Option<String> {
        args.windows(2)
            .find(|w| w[0] == flag)
            .map(|w| w[1].clone())
            .or_else(|| env::var(env_key).ok())
    };

    let graph_path = get_arg("--graph-basename", "GRAPH_BASENAME")
        .unwrap_or_else(|| "/dev/shm/swh-graph/current/graph".to_string());
    let suffix = get_arg("--suffix", "DATASET_SUFFIX")
        .unwrap_or_else(|| "full_2025-10_v2".to_string());
    let db_path = get_arg("--db-path", "DB_PATH")
        .unwrap_or_else(|| format!("data/tags_alterations_{}.db", suffix));

    println!("Loading graph...");
    let graph = SwhBidirectionalGraph::new(graph_path)?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;

    let start = Instant::now();
    println!("Querying database...");
    let mut conn = Connection::open(db_path)?;
    let table_exists = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='tags_with_deletion_creation_detection'")
        .and_then(|mut stmt| stmt.exists([]))
        .unwrap_or(false);

    if !table_exists {
        println!("Table doesn't exist");
        return Ok(());
    }

    // todo!("query tags_with_deletion_creation_detection instead with a WHERE condidtion");
    let min_date = NaiveDateTime::parse_from_str("2016-02-23 00:00:00", "%Y-%m-%d %H:%M:%S")
        .unwrap()
        .and_utc()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let mut stmt = conn.prepare(
        "SELECT
            origin_url,
            tag_name,
            type,
            old_snapshot,
            old_snap_timestamp,
            old_revision,
            old_root_dir,
            new_snapshot,
            new_snap_timestamp,
            creation_type,
            category,
            status,
            creation_snapshot,
            new_revision,
            new_root_dir,
            creation_rev,
            creation_root_dir
            FROM tags_with_deletion_creation_detection
            WHERE (status != 'non-legit' OR status IS NULL) AND (creation_root_dir IS NOT NULL OR new_root_dir IS NOT NULL) AND old_snap_timestamp >= ?1"
    )?;

    // let mut stmt = conn.prepare(
    //     "SELECT 
    //         ti.origin_url,
    //         ti.tag_name,
    //         ti.type,
    //         ti.old_snapshot,
    //         ti.old_snap_timestamp,
    //         ti.old_revision,
    //         ti.old_root_dir,
    //         ti.new_snapshot,
    //         ti.new_snap_timestamp,
    //         dc.creation_type,
    //         CASE 
    //             WHEN ti.new_revision IS NOT NULL THEN 'Move'
    //             ELSE 'Deletion'
    //         END AS category,
    //         CASE 
    //             WHEN dc.creation_delta = dc.new_snap_timestamp
    //                  AND ti.type = 'lightweight'
    //                  AND dc.creation_type = 'annotated'
    //                  AND ti.old_snap_timestamp < 1442534400
    //             THEN 'non-legit'
    //             WHEN dc.creation_delta IS NOT NULL THEN 'legit'
    //             ELSE NULL
    //         END AS status,
    //         dc.creation_snapshot,
    //         ti.new_revision,
    //         ti.new_root_dir,
    //         dc.creation_rev,
    //         dc.creation_root_dir
    //     FROM tag_inconsistencies ti
    //     LEFT JOIN deletion_creation_v2 dc
    //         ON ti.origin_url = dc.origin_url
    //         AND ti.tag_name = dc.tag_name
    //         AND ti.type = dc.type
    //         AND ti.old_snapshot = dc.old_snapshot
    //         AND ti.old_snap_timestamp = dc.old_snap_timestamp
    //     WHERE category = 'Move' OR status = 'legit'",
    // )?;

    let rows: Vec<TagAlteration> = stmt
        .query_map([&min_date], |row| {
            Ok(TagAlteration {
                origin_url: row.get(0)?,
                tag_name: row.get(1)?,
                type_: row.get(2)?,
                old_snapshot: row.get(3)?,
                old_snap_timestamp: NaiveDateTime::parse_from_str(&row.get::<_, String>(4)?, "%Y-%m-%d %H:%M:%S").map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e)))?.and_utc().timestamp(),
                old_revision: row.get(5)?,
                old_root_dir: row.get(6)?,
                new_snapshot: row.get(7)?,
                new_snap_timestamp: NaiveDateTime::parse_from_str(&row.get::<_, String>(8)?, "%Y-%m-%d %H:%M:%S").map_err(|e| rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(e)))?.and_utc().timestamp(),
                creation_type: row.get(9)?,
                category: row.get(10)?,
                status: row.get(11)?,
                creation_snapshot: row.get(12)?,
                new_revision: row.get(13)?,
                new_root_dir: row.get(14)?,
                creation_rev: row.get(15)?,
                creation_root_dir: row.get(16)?,
            })
        })?
        .filter_map(|r| match r {
            Ok(row) => Some(row),
            Err(e) => {
                eprintln!("Row error: {e}");
                None
            }
        })
        .collect();

    drop(stmt); // Release the borrow on conn

    println!("Done retrieving in: {:?}", start.elapsed());

    let moves = rows.iter().filter(|r| r.category == "Move").count();
    let deletions = rows.iter().filter(|r| r.category == "Deletion").count();

    println!(
        "Retrieved {} total rows: {} moves, {} legit deletions",
        rows.len().to_formatted_string(&*FORMAT),
        moves.to_formatted_string(&*FORMAT),
        deletions.to_formatted_string(&*FORMAT)
    );

    println!("Classifying...");
    let start = Instant::now();

    let pb = ProgressBar::new(rows.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {percent} ({eta})")
            .unwrap()
            .progress_chars("██░"),
    );
    let results: Vec<(TagAlteration, Classified)> = rows
        .into_par_iter()
        .filter_map(|row| {
            pb.inc(1);
            let Some(type_out) = get_type(&row) else {
                NO_TYPES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return None;
            };
            let Some(node_types) = get_node_types(&row, &graph) else {
                NO_RELS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return None;
            };
            let diffs = compute_diffs(&row, type_out, node_types, &graph);
            Some((row, diffs))
        })
        .collect();

    pb.finish();
    println!("Done Classifying: {:?}", start.elapsed());

    display_counters();

    println!("\nWriting results to database...");
    let write_start = Instant::now();
    write_diffs_to_db(&mut conn, &results)?;
    println!("Done writing to database: {:?}", write_start.elapsed());

    Ok(())
}

fn compute_diffs<G: SwhFullGraph>(
    ta: &TagAlteration,
    type_out: String,
    releases: Releases,
    graph: &G,
) -> Classified {
    let mut diffs = Classified {
        diff_rel: None,
        diff_rev: None,
        diff_dir: None,
        old_release_swhid: releases
            .rel_in
            .map(|id| graph.properties().swhid(id).to_string()),
        new_release_swhid: releases
            .rel_out
            .map(|id| graph.properties().swhid(id).to_string()),
        old_revision_swhid: ta.old_revision.clone(),
        new_revision_swhid: ta.new_revision.clone().or(ta.creation_rev.clone()),
        old_directory_swhid: ta.old_root_dir.clone(),
        new_directory_swhid: ta.new_root_dir.clone().or(ta.creation_root_dir.clone()),
    };

    if let Some(old_dir) = ta.old_root_dir.as_ref() {
        if let Some(new_dir) = ta.new_root_dir.as_ref() {
            if old_dir != new_dir {
                compute_diff_dir(&old_dir, &new_dir, &mut diffs, graph);
            }
        } else if let Some(new_dir) = ta.creation_root_dir.as_ref() {
            if old_dir != new_dir {
                compute_diff_dir(&old_dir, &new_dir, &mut diffs, graph);
            }
        }
    } else {
        todo!("count");
    }

    if let Some(old_rev) = ta.old_revision.as_ref() {
        if let Some(new_rev) = ta.new_revision.as_ref() {
            if old_rev != new_rev {
                compute_diff_rev(&old_rev, &new_rev, &mut diffs, graph);
            }
        } else if let Some(new_rev) = ta.creation_rev.as_ref() {
            if old_rev != new_rev {
                compute_diff_rev(&old_rev, &new_rev, &mut diffs, graph);
            }
        }
    } else {
        todo!("count");
    }

    if ta.type_ == "annotated" && ta.type_ == type_out {
        if releases.rel_in.is_some() && releases.rel_out.is_some() {
            if releases.rel_in.unwrap() != releases.rel_out.unwrap() {
                compute_diff_rel(
                    releases.rel_in.unwrap(),
                    releases.rel_out.unwrap(),
                    &mut diffs,
                    graph,
                );
            }
        } else {
            ANNOTATED_WITHOUT_REL.fetch_add(1, Ordering::Relaxed);
        }
    }

    diffs
}

fn compute_diff_rev<G: SwhFullGraph>(
    old_rev: &str,
    new_rev: &str,
    diffs: &mut Classified,
    graph: &G,
) {
    let old_rev_node = match graph.properties().node_id(old_rev) {
        Ok(node) => node,
        Err(_) => {
            REV_NODE_ID_ERR.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    let new_rev_node = match graph.properties().node_id(new_rev) {
        Ok(node) => node,
        Err(_) => {
            REV_NODE_ID_ERR.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    let message_differs = compare_messages(old_rev_node, new_rev_node, graph);
    let author_differs = compare_authors(old_rev_node, new_rev_node, graph);
    let committer_differs = compare_committers(old_rev_node, new_rev_node, graph);
    let author_timestamp_differs = compare_author_timestamps(old_rev_node, new_rev_node, graph);
    let committer_timestamp_differs =
        compare_committer_timestamps(old_rev_node, new_rev_node, graph);

    diffs.diff_rev = Some(DiffRevision {
        message_differs,
        author_differs,
        committer_differs,
        author_timestamp_differs,
        committer_timestamp_differs,
    });
}

fn compare_messages<G: SwhFullGraph>(old_rev: usize, new_rev: usize, graph: &G) -> bool {
    let old_msg = graph.properties().message(old_rev);
    let new_msg = graph.properties().message(new_rev);

    match (old_msg, new_msg) {
        (Some(msg1), Some(msg2)) => {
            let msg1_str = String::from_utf8_lossy(&msg1);
            let msg2_str = String::from_utf8_lossy(&msg2);
            msg1_str != msg2_str
        }
        (Some(_), None) | (None, Some(_)) => true,
        (None, None) => false,
    }
}

fn compare_authors<G: SwhFullGraph>(old_rev: usize, new_rev: usize, graph: &G) -> bool {
    let old_author = graph.properties().author_id(old_rev);
    let new_author = graph.properties().author_id(new_rev);

    match (old_author, new_author) {
        (Some(a1), Some(a2)) => a1 != a2,
        (Some(_), None) | (None, Some(_)) => true,
        (None, None) => false,
    }
}

fn compare_committers<G: SwhFullGraph>(old_rev: usize, new_rev: usize, graph: &G) -> bool {
    let old_committer = graph.properties().committer_id(old_rev);
    let new_committer = graph.properties().committer_id(new_rev);

    match (old_committer, new_committer) {
        (Some(c1), Some(c2)) => c1 != c2,
        (Some(_), None) | (None, Some(_)) => true,
        (None, None) => false,
    }
}

fn compare_author_timestamps<G: SwhFullGraph>(old_rev: usize, new_rev: usize, graph: &G) -> bool {
    let old_ts = graph.properties().author_timestamp(old_rev);
    let new_ts = graph.properties().author_timestamp(new_rev);

    match (old_ts, new_ts) {
        (Some(t1), Some(t2)) => t1 != t2,
        (Some(_), None) | (None, Some(_)) => true,
        (None, None) => false,
    }
}

fn compare_committer_timestamps<G: SwhFullGraph>(
    old_rev: usize,
    new_rev: usize,
    graph: &G,
) -> bool {
    let old_ts = graph.properties().committer_timestamp(old_rev);
    let new_ts = graph.properties().committer_timestamp(new_rev);

    match (old_ts, new_ts) {
        (Some(t1), Some(t2)) => t1 != t2,
        (Some(_), None) | (None, Some(_)) => true,
        (None, None) => false,
    }
}

fn compute_diff_dir<G: SwhFullGraph>(
    old_dir: &str,
    new_dir: &str,
    diffs: &mut Classified,
    graph: &G,
) {
    let old_dir_node = match graph.properties().node_id(old_dir) {
        Ok(node) => node,
        Err(_) => {
            DIR_NODE_ID_ERR.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    let new_dir_node = match graph.properties().node_id(new_dir) {
        Ok(node) => node,
        Err(_) => {
            DIR_NODE_ID_ERR.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    let tree_diff = match diff::tree_diff_dirs(graph, Some(old_dir_node), new_dir_node, None) {
        Ok(diff) => diff,
        Err(_) => {
            TREE_DIFF_ERR.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    let mut added_files: HashMap<String, usize> = HashMap::new();
    let mut deleted_files: HashMap<String, usize> = HashMap::new();
    let mut modified_files: Vec<String> = Vec::new();

    for op in tree_diff.operations() {
        match op {
            diff::TreeDiffOperation::Added { path, new_file, .. } => {
                let path_str = path_to_string(path.as_ref(), graph);
                added_files.insert(path_str, new_file);
            }
            diff::TreeDiffOperation::Deleted { path, old_file, .. } => {
                let path_str = path_to_string(path.as_ref(), graph);
                deleted_files.insert(path_str, old_file);
            }
            diff::TreeDiffOperation::Modified { path, .. } => {
                let path_str = path_to_string(path.as_ref(), graph);
                modified_files.push(path_str);
            }
            diff::TreeDiffOperation::Moved { .. } => {
                // stdlib doesn't detect moves, handle this below
            }
        }
    }

    let mut renamed_count = 0;
    let deleted_hashes: HashMap<usize, BTreeSet<String>> = {
        let mut map = HashMap::new();
        for (path, &node_id) in &deleted_files {
            map.entry(node_id)
                .or_insert_with(BTreeSet::new)
                .insert(path.clone());
        }
        map
    };

    let mut final_added = added_files.clone();
    for (added_path, added_node_id) in added_files.into_iter() {
        if let Some(deleted_paths) = deleted_hashes.get(&added_node_id) {
            // Same content, different path = rename
            renamed_count += 1;
            final_added.remove(&added_path);
            // Remove one instance of this hash from deleted and add to modified
            if let Some(path_to_remove) = deleted_paths.iter().next() {
                deleted_files.remove(path_to_remove);
                // Add the old path (before rename) to modified_files
                modified_files.push(path_to_remove.clone());
            }
        }
    }

    diffs.diff_dir = Some(DiffDirectory {
        added: final_added.len(),
        deleted: deleted_files.len(),
        modified: modified_files.len(),
        renamed: renamed_count,
        added_files: final_added.keys().cloned().collect(),
        deleted_files: deleted_files.keys().cloned().collect(),
        modified_files,
    });
}

fn path_to_string<G: SwhFullGraph>(path: &[swh_graph::labels::LabelNameId], graph: &G) -> String {
    use std::sync::atomic::Ordering;

    path.iter()
        .filter_map(|&label_id| {
            String::from_utf8(graph.properties().label_name(label_id))
                .map_err(|_| DIR_NAME_UTF8_ERR.fetch_add(1, Ordering::Relaxed))
                .ok()
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn compute_diff_rel<G: SwhFullGraph>(
    rel_in: usize,
    rel_out: usize,
    diffs: &mut Classified,
    graph: &G,
) {
    let message_differs = compare_release_messages(rel_in, rel_out, graph);
    let author_differs = compare_release_authors(rel_in, rel_out, graph);
    let author_timestamp_differs = compare_release_author_timestamps(rel_in, rel_out, graph);

    diffs.diff_rel = Some(DiffRelease {
        message_differs,
        author_differs,
        author_timestamp_differs,
    });
}

fn compare_release_messages<G: SwhFullGraph>(rel_in: usize, rel_out: usize, graph: &G) -> bool {
    let old_msg = graph.properties().message(rel_in);
    let new_msg = graph.properties().message(rel_out);

    match (old_msg, new_msg) {
        (Some(msg1), Some(msg2)) => {
            let msg1_str = String::from_utf8_lossy(&msg1);
            let msg2_str = String::from_utf8_lossy(&msg2);
            msg1_str != msg2_str
        }
        (Some(_), None) | (None, Some(_)) => {
            REL_MESSAGE_ERR.fetch_add(1, Ordering::Relaxed);
            true
        }
        (None, None) => {
            REL_MESSAGE_ERR.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

fn compare_release_authors<G: SwhFullGraph>(rel_in: usize, rel_out: usize, graph: &G) -> bool {
    let old_author = graph.properties().author_id(rel_in);
    let new_author = graph.properties().author_id(rel_out);

    match (old_author, new_author) {
        (Some(a1), Some(a2)) => a1 != a2,
        (Some(_), None) | (None, Some(_)) => {
            REL_AUTHOR_ERR.fetch_add(1, Ordering::Relaxed);
            true
        }
        (None, None) => {
            REL_AUTHOR_ERR.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

fn compare_release_author_timestamps<G: SwhFullGraph>(
    rel_in: usize,
    rel_out: usize,
    graph: &G,
) -> bool {
    let old_ts = graph.properties().author_timestamp(rel_in);
    let new_ts = graph.properties().author_timestamp(rel_out);

    match (old_ts, new_ts) {
        (Some(t1), Some(t2)) => t1 != t2,
        (Some(_), None) | (None, Some(_)) => {
            REL_AUTHOR_TIMESTAMP_ERR.fetch_add(1, Ordering::Relaxed);
            true
        }
        (None, None) => {
            REL_AUTHOR_TIMESTAMP_ERR.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

fn get_type(ta: &TagAlteration) -> Option<String> {
    if ta.category == "Move".to_string() {
        return Some(ta.type_.clone());
    }
    if ta.status.is_some() {
        return Some(ta.creation_type.as_ref().unwrap().clone());
    }
    None
}

struct Releases {
    rel_in: Option<usize>,
    rel_out: Option<usize>,
}

fn get_node_types<G: SwhFullGraph>(ta: &TagAlteration, graph: &G) -> Option<Releases> {
    let mut rels = Releases {
        rel_in: None,
        rel_out: None,
    };
    let snap_node_in = graph.properties().node_id(ta.old_snapshot.as_str()).ok()?;
    'glob: for (succ, labels) in graph.labeled_successors(snap_node_in) {
        for label in labels {
            if let EdgeLabel::Branch(branch) = label {
                let Ok(branch_name) =
                    String::from_utf8(graph.properties().label_name(branch.label_name_id()))
                else {
                    continue;
                };
                if branch_name == ta.tag_name {
                    if graph.properties().node_type(succ) == NodeType::Release {
                        rels.rel_in = Some(succ);
                        break 'glob;
                    }
                }
            }
        }
        // depth + 1 --> case Release --> Release --> Revision
        for (succ_bis, labels) in graph.labeled_successors(succ) {
            for label in labels {
                if let EdgeLabel::Branch(branch) = label {
                    let Ok(branch_name) =
                        String::from_utf8(graph.properties().label_name(branch.label_name_id()))
                    else {
                        continue;
                    };
                    if branch_name == ta.tag_name {
                        if graph.properties().node_type(succ_bis) == NodeType::Release {
                            rels.rel_in = Some(succ);
                            break 'glob;
                        }
                    }
                }
            }
        }
    }

    let snap_node_out = graph.properties().node_id(ta.new_snapshot.as_str()).ok()?;
    'glob: for (succ, labels) in graph.labeled_successors(snap_node_out) {
        for label in labels {
            if let EdgeLabel::Branch(branch) = label {
                let Ok(branch_name) =
                    String::from_utf8(graph.properties().label_name(branch.label_name_id()))
                else {
                    continue;
                };
                if branch_name == ta.tag_name {
                    if graph.properties().node_type(succ) == NodeType::Release {
                        rels.rel_out = Some(succ);
                        break 'glob;
                    }
                }
            }
        }
        // depth + 1 --> case Release --> Release --> Revision
        for (succ_bis, labels) in graph.labeled_successors(succ) {
            for label in labels {
                if let EdgeLabel::Branch(branch) = label {
                    let Ok(branch_name) =
                        String::from_utf8(graph.properties().label_name(branch.label_name_id()))
                    else {
                        continue;
                    };
                    if branch_name == ta.tag_name {
                        if graph.properties().node_type(succ_bis) == NodeType::Release {
                            rels.rel_out = Some(succ);
                            break 'glob;
                        }
                    }
                }
            }
        }
    }
    return Some(rels);
}

fn format_counters() -> String {
    use std::sync::atomic::Ordering;

    let no_types = NO_TYPES.load(Ordering::Relaxed);
    let no_rels = NO_RELS.load(Ordering::Relaxed);
    let types = TYPES.load(Ordering::Relaxed);
    let rels = RELS.load(Ordering::Relaxed);
    let dir_node_id_err = DIR_NODE_ID_ERR.load(Ordering::Relaxed);
    let dir_name_utf8_err = DIR_NAME_UTF8_ERR.load(Ordering::Relaxed);
    let rev_node_id_err = REV_NODE_ID_ERR.load(Ordering::Relaxed);
    let rel_message_err = REL_MESSAGE_ERR.load(Ordering::Relaxed);
    let rel_author_err = REL_AUTHOR_ERR.load(Ordering::Relaxed);
    let rel_author_timestamp_err = REL_AUTHOR_TIMESTAMP_ERR.load(Ordering::Relaxed);

    format!(
        "Classification Results:\n\
         ----------------------\n\
         Types found: {}\n\
         No types: {}\n\
         Releases found: {}\n\
         No releases: {}\n\
         Directory node ID errors: {}\n\
         Directory name UTF-8 errors: {}\n\
         Tree diff errs: {}\n\
         Revision node ID errors: {}\n\
         Annotated that aren't releases: {}\n\
         Release message errors: {}\n\
         Release author errors: {}\n\
         Release author timestamp errors: {}\n",
        types.to_formatted_string(&*FORMAT),
        no_types.to_formatted_string(&*FORMAT),
        rels.to_formatted_string(&*FORMAT),
        no_rels.to_formatted_string(&*FORMAT),
        dir_node_id_err.to_formatted_string(&*FORMAT),
        dir_name_utf8_err.to_formatted_string(&*FORMAT),
        TREE_DIFF_ERR
            .load(Ordering::Relaxed)
            .to_formatted_string(&*FORMAT),
        rev_node_id_err.to_formatted_string(&*FORMAT),
        ANNOTATED_WITHOUT_REL
            .load(Ordering::Relaxed)
            .to_formatted_string(&*FORMAT),
        rel_message_err.to_formatted_string(&*FORMAT),
        rel_author_err.to_formatted_string(&*FORMAT),
        rel_author_timestamp_err.to_formatted_string(&*FORMAT),
    )
}

fn display_counters() {
    println!("\n{}", format_counters());
}

fn write_diffs_to_db(conn: &mut Connection, results: &[(TagAlteration, Classified)]) -> Result<()> {
    conn.execute("DROP TABLE IF EXISTS tag_diffs", [])?;
    conn.execute(
        "CREATE TABLE tag_diffs (
            origin_url TEXT NOT NULL,
            tag_name TEXT NOT NULL,
            tag_type TEXT NOT NULL,
            old_snapshot TEXT NOT NULL,
            altering_snapshot TEXT NOT NULL,
            old_snap_timestamp INTEGER NOT NULL,
            old_release_swhid TEXT,
            new_release_swhid TEXT,
            old_revision_swhid TEXT,
            new_revision_swhid TEXT,
            old_directory_swhid TEXT,
            new_directory_swhid TEXT,
            rel_message_differs INTEGER,
            rel_author_differs INTEGER,
            rel_author_timestamp_differs INTEGER,
            rev_message_differs INTEGER,
            rev_author_differs INTEGER,
            rev_committer_differs INTEGER,
            rev_author_timestamp_differs INTEGER,
            rev_committer_timestamp_differs INTEGER,
            dir_added INTEGER,
            dir_deleted INTEGER,
            dir_modified INTEGER,
            dir_renamed INTEGER,
            PRIMARY KEY (origin_url, tag_name, tag_type, old_snapshot, old_snap_timestamp)
        )",
        [],
    )?;

    conn.execute("DROP TABLE IF EXISTS tag_file_diffs", [])?;
    conn.execute(
        "CREATE TABLE tag_file_diffs (
            origin_url TEXT NOT NULL,
            tag_name TEXT NOT NULL,
            tag_type TEXT NOT NULL,
            old_snapshot TEXT NOT NULL,
            old_snap_timestamp INTEGER NOT NULL,
            altering_snapshot TEXT NOT NULL,
            new_snap_timestamp INTEGER NOT NULL,
            old_directory_swhid TEXT,
            new_directory_swhid TEXT,
            file_path TEXT NOT NULL,
            change_type TEXT NOT NULL
        )",
        [],
    )?;

    // Use a transaction for much faster batch inserts
    let tx = conn.transaction()?;

    let mut stmt_diffs = tx.prepare(
        "INSERT OR REPLACE INTO tag_diffs (
            origin_url, tag_name, tag_type, old_snapshot, altering_snapshot, old_snap_timestamp,
            old_release_swhid, new_release_swhid, old_revision_swhid, new_revision_swhid,
            old_directory_swhid, new_directory_swhid,
            rel_message_differs, rel_author_differs, rel_author_timestamp_differs,
            rev_message_differs, rev_author_differs, rev_committer_differs,
            rev_author_timestamp_differs, rev_committer_timestamp_differs,
            dir_added, dir_deleted, dir_modified, dir_renamed
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
    )?;

    let mut stmt_files = tx.prepare(
        "INSERT INTO tag_file_diffs (origin_url, tag_name, tag_type, old_snapshot, old_snap_timestamp, altering_snapshot, new_snap_timestamp, old_directory_swhid, new_directory_swhid, file_path, change_type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;

    let pb = ProgressBar::new(results.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {percent} ({eta})")
            .unwrap()
            .progress_chars("██░"),
    );

    for (ta, classified) in results {
        // Use new_snapshot if Move, creation_snapshot if legit deletion
        let snapshot_to_use = if ta.category == "Move" {
            &ta.new_snapshot
        } else {
            ta.creation_snapshot.as_ref().unwrap_or(&ta.new_snapshot)
        };

        stmt_diffs.execute(rusqlite::params![
            &ta.origin_url,
            &ta.tag_name,
            &ta.type_,
            &ta.old_snapshot,
            snapshot_to_use,
            ta.old_snap_timestamp,
            &classified.old_release_swhid,
            &classified.new_release_swhid,
            &classified.old_revision_swhid,
            &classified.new_revision_swhid,
            &classified.old_directory_swhid,
            &classified.new_directory_swhid,
            classified
                .diff_rel
                .as_ref()
                .map(|d| d.message_differs as i32),
            classified
                .diff_rel
                .as_ref()
                .map(|d| d.author_differs as i32),
            classified
                .diff_rel
                .as_ref()
                .map(|d| d.author_timestamp_differs as i32),
            classified
                .diff_rev
                .as_ref()
                .map(|d| d.message_differs as i32),
            classified
                .diff_rev
                .as_ref()
                .map(|d| d.author_differs as i32),
            classified
                .diff_rev
                .as_ref()
                .map(|d| d.committer_differs as i32),
            classified
                .diff_rev
                .as_ref()
                .map(|d| d.author_timestamp_differs as i32),
            classified
                .diff_rev
                .as_ref()
                .map(|d| d.committer_timestamp_differs as i32),
            classified.diff_dir.as_ref().map(|d| d.added as i32),
            classified.diff_dir.as_ref().map(|d| d.deleted as i32),
            classified.diff_dir.as_ref().map(|d| d.modified as i32),
            classified.diff_dir.as_ref().map(|d| d.renamed as i32),
        ])?;

        if let Some(ref dir_diff) = classified.diff_dir {
            for file in &dir_diff.added_files {
                stmt_files.execute(rusqlite::params![
                    &ta.origin_url,
                    &ta.tag_name,
                    &ta.type_,
                    &ta.old_snapshot,
                    ta.old_snap_timestamp,
                    snapshot_to_use,
                    ta.new_snap_timestamp,
                    &classified.old_directory_swhid,
                    &classified.new_directory_swhid,
                    file,
                    "added"
                ])?;
            }
            for file in &dir_diff.deleted_files {
                stmt_files.execute(rusqlite::params![
                    &ta.origin_url,
                    &ta.tag_name,
                    &ta.type_,
                    &ta.old_snapshot,
                    ta.old_snap_timestamp,
                    snapshot_to_use,
                    ta.new_snap_timestamp,
                    &classified.old_directory_swhid,
                    &classified.new_directory_swhid,
                    file,
                    "deleted"
                ])?;
            }
            for file in &dir_diff.modified_files {
                stmt_files.execute(rusqlite::params![
                    &ta.origin_url,
                    &ta.tag_name,
                    &ta.type_,
                    &ta.old_snapshot,
                    ta.old_snap_timestamp,
                    snapshot_to_use,
                    ta.new_snap_timestamp,
                    &classified.old_directory_swhid,
                    &classified.new_directory_swhid,
                    file,
                    "modified"
                ])?;
            }
        }
        pb.inc(1);
    }
    pb.finish();

    drop(stmt_diffs);
    drop(stmt_files);

    // Commit the transaction
    tx.commit()?;

    Ok(())
}
