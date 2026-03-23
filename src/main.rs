use anyhow::{Result, anyhow};
use dotenv::dotenv;
use std::env;
use swh_graph::graph::*;
use swh_graph::mph::DynMphf;

fn cli_or_env(key: &str, env_key: &str) -> Option<String> {
    let args: Vec<String> = env::args().collect();
    args.windows(2)
        .find(|w| w[0] == key)
        .map(|w| w[1].clone())
        .or_else(|| env::var(env_key).ok())
}

fn required_cli_or_env(key: &str, env_key: &str) -> Result<String> {
    cli_or_env(key, env_key).ok_or_else(|| {
        anyhow!(
            "Missing required input. Provide {} <value> or set {}",
            key,
            env_key
        )
    })
}

fn main() -> Result<()> {
    dotenv()?;
    let graph_basename = required_cli_or_env("--graph-basename", "GRAPH_BASENAME")?;
    let orc_dir = required_cli_or_env("--orc-dir", "ORC_DIR")?;
    let suffix = cli_or_env("--suffix", "DATASET_SUFFIX")
        .unwrap_or_else(|| "full_2025-10_v2".to_string());
    let db_path = cli_or_env("--db-path", "DB_PATH")
        .unwrap_or_else(|| format!("data/tags_alterations_{}.db", suffix));
    let graph = SwhBidirectionalGraph::new(graph_basename)?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;
    tags_alterations::tags_check_full(&graph, &suffix, &orc_dir, &db_path)?;
    Ok(())
}
