use anyhow::Result;
use dotenv::dotenv;
use std::env;
use swh_graph::graph::*;
use swh_graph::mph::DynMphf;
fn main() -> Result<()> {
    dotenv()?;
    let graph_basename = env::var("GRAPH_BASENAME").expect("GRAPH_BASENAME not set");
    let graph = SwhBidirectionalGraph::new(graph_basename)?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;
    Ok(())
}
