use anyhow::Result;
use rusqlite::Connection;

use swh_graph::graph::*;
use swh_graph::mph::DynMphf;

pub fn main() -> Result<()> {
    let graph = SwhBidirectionalGraph::new("/dev/shm/swh-graph/current/graph")?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;
    let conn = Connection::open(format!("data/tags_alterations_full_2025-10_v2.db"))?;

    tags_alterations::extensions::release_timestamp_anomalies(&graph, &conn)?;

    drop(conn);
    Ok(())
}
