use std::net::SocketAddrV4;
use std::sync::Arc;

use anyhow::{Context, Result};
use everscale_network::NetworkBuilder;
use everscale_network::{adnl, dht, overlay};
use global_config::GlobalConfig;
use rustc_hash::FxHashSet;

pub use geo_data::*;

mod geo_data;

pub async fn search_nodes(addr: SocketAddrV4, global_config: GlobalConfig) -> Result<NodeIps> {
    tracing::info!(%addr, "using public ip");

    let (_adnl, dht) = NetworkBuilder::with_adnl(
        addr,
        adnl::Keystore::builder()
            .with_tagged_key(generate_key(), 0)?
            .build(),
        Default::default(),
    )
    .with_dht(0, Default::default())
    .build()?;

    let file_hash = global_config.zero_state.file_hash;

    let mc_overlay_id =
        overlay::IdFull::for_workchain_overlay(-1, file_hash.as_slice()).compute_short_id();
    let sc_overlay_id =
        overlay::IdFull::for_workchain_overlay(0, file_hash.as_slice()).compute_short_id();

    for dht_node in global_config.dht_nodes {
        dht.add_dht_peer(dht_node)?;
    }
    dht.find_more_dht_nodes()
        .await
        .context("failed to find more DHT nodes")?;

    tracing::info!(
        node_count = dht.iter_known_peers().count(),
        "found DHT nodes"
    );

    // Search overlay peers
    let mut nodes = NodeIps::default();
    for _ in 0..10 {
        scan_overlay(&dht, &mc_overlay_id, &mut nodes)
            .await
            .context("Failed to scan overlay -1")?;
        scan_overlay(&dht, &sc_overlay_id, &mut nodes)
            .await
            .context("Failed to scan overlay 0")?;

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    // Done
    Ok(nodes)
}

async fn scan_overlay(
    dht: &Arc<dht::Node>,
    overlay_id: &overlay::IdShort,
    node_ips: &mut NodeIps,
) -> Result<()> {
    tracing::info!(%overlay_id, "scanning overlay");

    let result = dht
        .find_overlay_nodes(overlay_id)
        .await
        .context("Failed to find overlay nodes")?;

    let prev_count = node_ips.len();
    for (ip, _) in result {
        node_ips.insert(ip);
    }

    tracing::info!(
        %overlay_id,
        total_nodes = node_ips.len(),
        new_nodes = node_ips.len() - prev_count,
        "found new overlay nodes",
    );

    Ok(())
}

fn generate_key() -> [u8; 32] {
    use rand::Rng;

    rand::thread_rng().gen()
}

type NodeIps = FxHashSet<SocketAddrV4>;
