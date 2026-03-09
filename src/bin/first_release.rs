use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use swh_graph::{NodeType, graph::*, labels::EdgeLabel, mph::DynMphf};

use chrono::{DateTime, Utc};
use rayon::prelude::*;

fn main() -> Result<()> {
    // Threshold: 2015-01-01 00:00:00 UTC
    let min_date_threshold = DateTime::parse_from_rfc3339("2015-01-01T00:00:00Z")
        .unwrap()
        .timestamp();
    println!(
        "Threshold date: 2015-01-01 00:00:00 UTC (timestamps must be > {})",
        min_date_threshold
    );

    println!("Loading graph...");
    let graph = SwhBidirectionalGraph::new("/dev/shm/swh-graph/current/graph")?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;
    println!("Graph size: {}", graph.num_nodes());
    println!("Collecting origins...");
    let origins = (0..graph.num_nodes())
        .into_par_iter()
        .filter(|node| graph.properties().node_type(*node) == NodeType::Origin)
        .collect::<Vec<usize>>();
    println!("Collected {} origins", origins.len());
    let pb = ProgressBar::new(origins.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {percent}% ({eta})")
            .unwrap()
            .progress_chars("██░"),
    );
    let result = origins
        .into_par_iter()
        .filter_map(|origin| {
            pb.inc(1);
            let mut min_release = None;
            for (snapshot, labels) in graph.labeled_successors(origin) {
                for label in labels {
                    if let EdgeLabel::Visit(visit) = label {
                        if visit.timestamp() < min_date_threshold as u64 {
                            continue;
                        }

                        for rel in graph.successors(snapshot) {
                            if graph.properties().node_type(rel) == NodeType::Release {
                                min_release = if let Some((_, current_ts, _)) = min_release {
                                    if visit.timestamp() < current_ts {
                                        Some((rel, visit.timestamp(), snapshot))
                                    } else {
                                        min_release
                                    }
                                } else {
                                    Some((rel, visit.timestamp(), snapshot))
                                };
                            }
                        }
                    }
                }
            }
            min_release
        })
        .reduce_with(|a, b| if a.1 < b.1 { a } else { b })
        .unwrap();
    pb.finish_with_message("Done");

    let datetime = DateTime::<Utc>::from_timestamp(result.1 as i64, 0)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap());
    println!("Smallest Timestamp: {}", result.1);
    println!(
        "Snapshot: {}",
        graph.properties().swhid(result.2).to_string()
    );
    println!(
        "Human-readable Date: {}",
        datetime.format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!("Release Node ID: {}", result.0);
    println!(
        "Release SWHID: {}",
        graph.properties().swhid(result.0).to_string()
    );

    Ok(())
}
