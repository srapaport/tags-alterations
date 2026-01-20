use std::collections::HashMap;
use std::fs::File;
use std::io::Write;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use swh_graph::labels::{EdgeLabel, VisitStatus};
use swh_graph::mph::DynMphf;
use swh_graph::{NodeType, graph::*};

pub fn main() -> Result<()> {
    let graph = SwhBidirectionalGraph::new("/swh/scratch/graph/2025-05-18/compressed/graph")?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;
    let urls = vec![
        "https://github.com/aws/aws-sdk-go-v2",
        "https://github.com/CleverRaven/Cataclysm-DDA",
        "https://gitlab.com/wireshark/wireshark",
        "https://gitlab.linphone.org/BC/public/liblinphone.git",
        "https://android.googlesource.com/platform/system/extras",
        "https://android.googlesource.com/platform/bionic",
        "https://bitbucket.org/apisnetworks/apnscp.git",
        "https://bitbucket.org/berkeleylab/metabat.git",
        "https://codeberg.org/dnkl/foot",
        "https://codeberg.org/dnkl/fcft",
    ];
    let mut tags = HashMap::new();
    urls.into_iter()
        .filter_map(|url| {
            let origin = graph
                .properties()
                .node_id(swh_graph::SWHID::from_origin_url(url))
                .ok()?;
            Some((url, origin))
        })
        .for_each(|origin| {
            tags.insert(origin.0, enum_tags(origin.1, &graph));
        });
    
    write_to_json(&tags)?;
    
    println!("Data written to data/tags_snapshot.json");
    Ok(())
}

fn enum_tags<G: SwhFullGraph>(origin: usize, graph: &G) -> Vec<(String, u64, u64)> {
    let mut tags_per_snapshot = vec![];
    let mut snapshots = vec![];
    for (succ, labels) in graph.labeled_successors(origin) {
        if graph.properties().node_type(succ) != NodeType::Snapshot {
            continue;
        }

        for label in labels {
            if let EdgeLabel::Visit(visit) = label {
                if visit.status() != VisitStatus::Full {
                    continue;
                }
                snapshots.push((succ, visit.timestamp()));
            }
        }
    }
    snapshots.sort_unstable_by_key(|snap| snap.1);
    snapshots.into_iter().for_each(|snap| {
        let mut amount_tags = 0;
        for (_, labels) in graph.labeled_successors(snap.0) {
            for label in labels {
                if let EdgeLabel::Branch(branch) = label {
                    let Ok(branch_name) =
                        String::from_utf8(graph.properties().label_name(branch.label_name_id()))
                    else {
                        continue;
                    };
                    if branch_name.contains("/tags/") {
                        amount_tags += 1;
                    }
                }
            }
        }
        let snap_swhid = graph.properties().swhid(snap.0).to_string();
        tags_per_snapshot.push((snap_swhid, snap.1, amount_tags));
    });
    tags_per_snapshot
}

#[derive(Serialize, Deserialize)]
struct TagSnapshot {
    origin_url: String,
    snapshot_swhid: String,
    timestamp: u64,
    tag_count: u64,
}

fn write_to_json(tags: &HashMap<&str, Vec<(String, u64, u64)>>) -> Result<()> {
    let mut records = Vec::new();
    for (url, snapshots) in tags.iter() {
        for (swhid, timestamp, tag_count) in snapshots.iter() {
            records.push(TagSnapshot {
                origin_url: url.to_string(),
                snapshot_swhid: swhid.clone(),
                timestamp: *timestamp,
                tag_count: *tag_count,
            });
        }
    }
    let json = serde_json::to_string_pretty(&records)?;
    let mut file = File::create("data/tags_snapshot.json")?;
    file.write_all(json.as_bytes())?;
    Ok(())
}

/*
GitHub most inconsistencies:
https://github.com/aws/aws-sdk-go-v2            3890156
https://github.com/CleverRaven/Cataclysm-DDA     180071

Gitlab most inconsistencies:
https://gitlab.com/wireshark/wireshark                   31909
https://gitlab.linphone.org/BC/public/liblinphone.git    20316

Android most inconsistencies:
https://android.googlesource.com/platform/system/extras    186288
https://android.googlesource.com/platform/bionic           177499

Bitbucket most inconsistencies:
https://bitbucket.org/apisnetworks/apnscp.git    960
https://bitbucket.org/berkeleylab/metabat.git    451

Codeberg most inconsistencies:
https://codeberg.org/dnkl/foot    593
https://codeberg.org/dnkl/fcft    125
*/
