use std::sync::LazyLock;

use anyhow::Result;
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
}

struct DiffRelease {}

struct DiffRevision {}

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

fn main() -> Result<()> {
    println!("Loading graph...");
    let graph = SwhBidirectionalGraph::new("/dev/shm/swh-graph/current/graph")?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;

    let start = Instant::now();
    println!("Querying database...");
    let conn = Connection::open(format!("data/tags_alterations_full_2025-10_v2.db"))?;
    let table_exists = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='tag_inconsistencies'")
        .and_then(|mut stmt| stmt.exists([]))
        .unwrap_or(false);

    if !table_exists {
        println!("Table doesn't exist");
        return Ok(());
    }

    let mut stmt = conn.prepare(
        "SELECT 
            ti.origin_url,
            ti.tag_name,
            ti.type,
            ti.old_snapshot,
            ti.old_snap_timestamp,
            ti.old_revision,
            ti.old_root_dir,
            ti.new_snapshot,
            ti.new_snap_timestamp,
            dc.creation_type,
            CASE 
                WHEN ti.new_revision IS NOT NULL THEN 'Move'
                ELSE 'Deletion'
            END AS category,
            CASE 
                WHEN dc.creation_delta = dc.new_snap_timestamp
                     AND ti.type = 'lightweight'
                     AND dc.creation_type = 'annotated'
                     AND ti.old_snap_timestamp < 1442534400
                THEN 'non-legit'
                WHEN dc.creation_delta IS NOT NULL THEN 'legit'
                ELSE NULL
            END AS status,
            ti.new_revision,
            ti.new_root_dir,
            dc.creation_rev,
            dc.creation_root_dir
        FROM tag_inconsistencies ti
        LEFT JOIN deletion_creation_v2 dc
            ON ti.origin_url = dc.origin_url
            AND ti.tag_name = dc.tag_name
            AND ti.type = dc.type
            AND ti.old_snapshot = dc.old_snapshot
            AND ti.old_snap_timestamp = dc.old_snap_timestamp
        WHERE category = 'Move' OR status = 'legit'",
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
                creation_type: row.get(9)?,
                category: row.get(10)?,
                status: row.get(11)?,
                new_revision: row.get(12)?,
                new_root_dir: row.get(13)?,
                creation_rev: row.get(14)?,
                creation_root_dir: row.get(15)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

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
    let diffs: Vec<Classified> = rows
        .into_par_iter()
        .filter_map(|row| {
            pb.inc(1);
            // let Some(type_out) = get_type(&row) else {
            //     NO_TYPES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            //     return None;
            // };
            let Some(node_types) = get_node_types(&row, &graph) else {
                NO_RELS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return None;
            };
            Some(compute_diffs(row, node_types, &graph))
        })
        .collect();

    println!("Done Classifying: {:?}", start.elapsed());

    display_counters();

    Ok(())
}

fn compute_diffs<G: SwhFullGraph>(ta: TagAlteration, releases: Releases, graph: &G) -> Classified {
    let mut diffs = Classified {
        diff_rel: None,
        diff_rev: None,
        diff_dir: None,
    };

    if let Some(old_dir) = ta.old_root_dir {
        if let Some(new_dir) = ta.new_root_dir {
            if old_dir != new_dir {
                compute_diff_dir(&old_dir, &new_dir, &mut diffs, graph);
            }
        } else if let Some(new_dir) = ta.creation_root_dir {
            if old_dir != new_dir {
                compute_diff_dir(&old_dir, &new_dir, &mut diffs, graph);
            }
        }
    } else {
        todo!("count");
    }

    if let Some(old_rev) = ta.old_revision {
        if let Some(new_rev) = ta.new_revision {
            if old_rev != new_rev {
                compute_diff_rev(&old_rev, &new_rev, &mut diffs, graph);
            }
        } else if let Some(new_rev) = ta.creation_rev {
            if old_rev != new_rev {
                compute_diff_rev(&old_rev, &new_rev, &mut diffs, graph);
            }
        }
    } else {
        todo!("count");
    }

    if releases.rel_in.is_some() || releases.rel_out.is_some() {
        if releases.rel_in != releases.rel_out {
            compute_diff_rel(releases.rel_in, releases.rel_out, &mut diffs, graph);
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
            // Remove one instance of this hash from deleted
            if let Some(path_to_remove) = deleted_paths.iter().next() {
                deleted_files.remove(path_to_remove);
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
    rel_in: Option<usize>,
    rel_out: Option<usize>,
    diffs: &mut Classified,
    graph: &G,
) {
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

    format!(
        "Classification Results:\n\
         ----------------------\n\
         Types found: {}\n\
         No types: {}\n\
         Releases found: {}\n\
         No releases: {}\n\
         Directory node ID errors: {}\n\
         Directory name UTF-8 errors: {}\n\
         Tree diff errs: {}\n",
        types.to_formatted_string(&*FORMAT),
        no_types.to_formatted_string(&*FORMAT),
        rels.to_formatted_string(&*FORMAT),
        no_rels.to_formatted_string(&*FORMAT),
        dir_node_id_err.to_formatted_string(&*FORMAT),
        dir_name_utf8_err.to_formatted_string(&*FORMAT),
        TREE_DIFF_ERR.load(Ordering::Relaxed).to_formatted_string(&*FORMAT),
    )
}

fn display_counters() {
    println!("\n{}", format_counters());
}
