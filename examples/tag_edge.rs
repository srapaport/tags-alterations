use anyhow::Result;
use dotenv::dotenv;
use std::env;
use swh_graph::labels::EdgeLabel;
use swh_graph::mph::DynMphf;
use swh_graph::graph::*;

fn main() -> Result<()> {
    dotenv()?;
    let graph_basename = env::var("GRAPH_BASENAME").expect("GRAPH_BASENAME not set");

    println!("Load graph...");
    let graph = SwhBidirectionalGraph::new(graph_basename)?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;

    println!("graph loaded");
    let snapshot_a = "swh:1:snp:f6bb037673aec9079d3d386a8f0cde13caa675fa";
    let snap_a_node = graph.properties().node_id(snapshot_a)?;

    let snapshot_l = "swh:1:snp:d10c567a401e6a134a99899681bb2ee90007be0d";
    let snap_l_node = graph.properties().node_id(snapshot_l)?;

    println!("getting infos for a ...\n");
    get_info(snap_a_node, &graph);
    println!("\ngetting infos for b ...\n");
    get_info(snap_l_node, &graph);
    Ok(())
}

fn get_info<G: SwhFullGraph>(node: usize, graph: &G) {
    for (succ, labels) in graph.labeled_successors(node) {
        for label in labels {
            if let EdgeLabel::Branch(branch) = label {
                let Ok(branch_name) =
                    String::from_utf8(graph.properties().label_name(branch.label_name_id()))
                else {
                    continue;
                };
                if branch_name.contains("/tags/") {
                    println!(
                        "succ: {}\ttag: {}",
                        graph.properties().swhid(succ).to_string(),
                        branch_name
                    );
                }
            }
        }
    }
}
