use std::net::SocketAddrV4;
use std::sync::Arc;

use anyhow::{Context, Result};
use everscale_network::utils::PackedSocketAddr;
use everscale_network::NetworkBuilder;
use everscale_network::{adnl, dht, overlay};
use futures::StreamExt;
use global_config::GlobalConfig;
use rustc_hash::FxHashSet;

pub use geo_data::*;

mod geo_data;

pub async fn search_nodes(address: SocketAddrV4, global_config: GlobalConfig) -> Result<NodeIps> {
    log::info!("Using public ip: {}", address);

    let (adnl, dht) = NetworkBuilder::with_adnl(
        address,
        adnl::Keystore::builder()
            .with_tagged_key(generate_key(), 0)?
            .build(),
        Default::default(),
    )
    .with_dht(0, Default::default())
    .build()?;

    let file_hash = global_config.zero_state.file_hash;

    let mc_overlay_id =
        overlay::IdFull::for_shard_overlay(-1, file_hash.as_slice()).compute_short_id();
    let sc_overlay_id =
        overlay::IdFull::for_shard_overlay(0, file_hash.as_slice()).compute_short_id();

    adnl.start().context("Failed to start ADNL")?;

    let mut static_nodes = Vec::new();
    for dht_node in global_config.dht_nodes {
        if let Some(peer_id) = dht.add_dht_peer(dht_node)? {
            static_nodes.push(peer_id);
        }
    }
    log::info!("Using {} static nodes", static_nodes.len());

    // Search one level of peers
    {
        let mut tasks = futures::stream::FuturesUnordered::new();
        for peer_id in static_nodes.iter().cloned() {
            let dht = &dht;
            tasks.push(async move {
                let res = dht.query_dht_nodes(&peer_id, 10, false).await;
                (peer_id, res)
            });
        }

        while let Some((peer_id, res)) = tasks.next().await {
            match res {
                Ok(nodes) => {
                    for node in nodes {
                        dht.add_dht_peer(node)?;
                    }
                }
                Err(e) => log::error!("Failed to get DHT nodes from {}: {:?}", peer_id, e),
            }
        }
    }

    let dht_node_count = dht.iter_known_peers().count();
    log::info!("Found {} DHT nodes", dht_node_count);

    // Search overlay peers
    let mut nodes = NodeIps::default();
    scan_overlay(&dht, &mc_overlay_id, &mut nodes)
        .await
        .context("Failed to scan overlay -1")?;
    scan_overlay(&dht, &sc_overlay_id, &mut nodes)
        .await
        .context("Failed to scan overlay 0")?;

    // Done
    Ok(nodes)
}

async fn scan_overlay(
    dht: &Arc<dht::Node>,
    overlay_id: &overlay::IdShort,
    node_ips: &mut NodeIps,
) -> Result<()> {
    log::info!("Scanning overlay {}", overlay_id);

    let result = dht
        .find_overlay_nodes(overlay_id)
        .await
        .context("Failed to find overlay nodes")?;

    let node_count = node_ips.len();

    for (ip, _) in result {
        node_ips.insert(ip);
    }

    log::info!(
        "Found {} new overlay nodes in overlay {}",
        node_ips.len() - node_count,
        overlay_id
    );

    Ok(())
}

fn generate_key() -> [u8; 32] {
    use rand::Rng;

    rand::thread_rng().gen()
}

type NodeIps = FxHashSet<PackedSocketAddr>;
