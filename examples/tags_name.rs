use anyhow::Result;
use swh_graph::{NodeType, graph::*};
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
        for (succ, labels) in graph.labeled_successors(snapshot.0) {
            for label in labels {
                if let EdgeLabel::Branch(branch) = label {
                    let branch_name =
                        String::from_utf8(graph.properties().label_name(branch.label_name_id()))?;
                    println!("branch name:\t{}\ttype:\t{}", branch_name, graph.properties().node_type(succ));
                    if graph.properties().node_type(succ) == NodeType::Release{
                        let mut succ_rev = 0;
                        for succ in graph.successors(succ){
                            if graph.properties().node_type(succ) == NodeType::Revision{
                                succ_rev+=1;
                            }
                        }
                        if succ_rev>1{
                            println!("Problem more than 1");
                        }
                        else{
                            println!("all good");
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
