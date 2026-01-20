use anyhow::{Result, anyhow};
use dotenv::dotenv;
use std::env;
use swh_graph::graph::*;
use swh_graph::mph::DynMphf;
fn main() -> Result<()> {
    dotenv()?;
    let graph_basename = env::var("GRAPH_BASENAME").expect("GRAPH_BASENAME not set");
    let amount_origins: u64 = env::var("ORIGINS")
        .expect("ORIGINS not set")
        .parse()
        .map_err(|e| anyhow!("Invalid ORIGINS value: {}", e))?;
    let graph = SwhBidirectionalGraph::new(graph_basename)?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;
    tags_alterations::tags_check_full(&graph, amount_origins, "full_2025-10")?;
    Ok(())
}
