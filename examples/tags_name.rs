use anyhow::Result;
use swh_graph::graph::*;
use swh_graph::labels::EdgeLabel;
use swh_graph::mph::DynMphf;

pub fn main() -> Result<()> {
    let graph = SwhBidirectionalGraph::new("/swh/scratch/graph/2025-05-18/compressed/graph")?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;
    let url = "https://github.com/torvalds/linux";
    let swhid = swh_graph::SWHID::from_origin_url(url);
    let node = graph.properties().node_id(swhid)?;
    if let Some(snapshot) = swh_graph_stdlib::find_latest_snp(&graph, node)? {
        for (_, labels) in graph.labeled_successors(snapshot.0) {
            for label in labels {
                if let EdgeLabel::Branch(branch) = label {
                    let branch_name =
                        String::from_utf8(graph.properties().label_name(branch.label_name_id()))?;
                    println!("branch name:\t{}", branch_name);
                }
            }
        }
    }
    Ok(())
}
