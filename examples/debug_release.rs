use anyhow::Result;
use swh_graph::graph::*;
use swh_graph::mph::DynMphf;

pub fn main() -> Result<()> {
    let graph = SwhBidirectionalGraph::new("/swh/scratch/graph/2025-05-18/compressed/graph")?
        .load_all_properties::<DynMphf>()?
        .load_forward_labels()?
        .load_backward_labels()?;

    let releases = ["swh:1:rel:341edc9c32967142729e729257d21e240eadd6d4"];

    // for release in releases {
    //     let node_id = graph.properties().node_id(release)?;
    //     println!("successors of {}", release);
    //     for succ in graph.successors(node_id) {
    //         println!("\t{}", graph.properties().swhid(succ).to_string());
    //     }
    // }

    // let releases_bis = [
    //     "swh:1:rel:2f9c2d1811335c2894638f4afff19f6f45371594",
    //     "swh:1:rel:341edc9c32967142729e729257d21e240eadd6d4",
    //     "swh:1:rel:2f9c2d1811335c2894638f4afff19f6f45371594",
    // ];
    // println!("================= Releases of release");
    // for release in releases_bis {
    //     let node_id = graph.properties().node_id(release)?;
    //     println!("successors of {}", release);
    //     for succ in graph.successors(node_id) {
    //         println!("\t{}", graph.properties().swhid(succ).to_string());
    //     }
    // }

    for release in releases {
        let node_id = graph.properties().node_id(release)?;
        let timestamp = graph.properties().author_timestamp(node_id).unwrap();
        let message = String::from_utf8(graph.properties().message(node_id).unwrap())?;
        let _author = graph.properties().author_id(node_id).unwrap();
        println!(
            "Found everything with timestamp: {} and message: {}",
            timestamp, message
        );
    }
    Ok(())
}
