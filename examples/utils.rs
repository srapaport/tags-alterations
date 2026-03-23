use anyhow::Result;
use std::env;
use swh_graph::labels::{EdgeLabel, VisitStatus};
use swh_graph::mph::DynMphf;
use swh_graph::{NodeType, graph::*};

fn main() -> Result<()> {
    let graph_path = env::var("GRAPH_BASENAME").map_err(|_| {
        anyhow::anyhow!("GRAPH_BASENAME is required for examples/utils.rs")
    })?;
    let graph = SwhBidirectionalGraph::new(graph_path)?
    .load_all_properties::<DynMphf>()?
    .load_forward_labels()?
    .load_backward_labels()?;

    let origin_url = "https://github.com/encode/uvicorn";
    let origin_swhid = swh_graph::SWHID::from_origin_url(origin_url);
    let origin_node = graph.properties().node_id(origin_swhid)?;

    let mut snapshots = vec![];
    for (succ, labels) in graph.labeled_successors(origin_node) {
        if graph.properties().node_type(succ) != NodeType::Snapshot {
            continue;
        }
        let succ_swhid = graph.properties().swhid(succ).to_string();
        for label in labels {
            if let EdgeLabel::Visit(visit) = label {
                if visit.status() == VisitStatus::Partial {
                    continue;
                }
                snapshots.push((succ_swhid.clone(), visit.timestamp()));
            }
        }
    }

    snapshots.sort_by_key(|snap| snap.1);
    snapshots.into_iter().for_each(|snap| {
        println!("SWHID:\t{}\tts:\t{}", snap.0, snap.1);
    });
    Ok(())
}
