use std::net::SocketAddrV4;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::StreamExt;
use global_config::*;
use tiny_adnl::utils::*;
use tiny_adnl::*;

pub use geo_data::*;

mod geo_data;

pub async fn search_nodes(address: SocketAddrV4, global_config: GlobalConfig) -> Result<NodeIps> {
    log::info!("Using public ip: {}", address);

    let adnl = AdnlNode::new(
        address.into(),
        AdnlKeystore::from_tagged_keys(vec![(generate_key(), 1), (generate_key(), 2)])?,
        AdnlNodeOptions {
            ..Default::default()
        },
        None,
    );

    let dht = DhtNode::new(
        adnl.clone(),
        1,
        DhtNodeOptions {
            ..Default::default()
        },
    )
    .context("Failed to create DHT node")?;

    let file_hash = global_config.zero_state.file_hash;

    let mc_overlay_id = compute_overlay_id(-1, 0, file_hash.into())?.compute_short_id()?;
    let sc_overlay_id = compute_overlay_id(0, 0, file_hash.into())?.compute_short_id()?;

    adnl.start(vec![dht.clone()])
        .await
        .context("Failed to start ADNL")?;

    let mut static_nodes = Vec::new();
    for dht_node in global_config.dht_nodes {
        if let Some(peer_id) = dht.add_peer(dht_node)? {
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
                let res = dht.find_dht_nodes(&peer_id).await;
                (peer_id, res)
            });
        }

        while let Some((peer_id, res)) = tasks.next().await {
            if let Err(e) = res {
                log::error!("Failed to get DHT nodes from {}: {:?}", peer_id, e);
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
    dht: &Arc<DhtNode>,
    overlay_id: &OverlayIdShort,
    node_ips: &mut NodeIps,
) -> Result<()> {
    log::info!("Scanning overlay {}", overlay_id);

    let mut iter = None;
    loop {
        let result = dht
            .find_overlay_nodes(overlay_id, &mut iter)
            .await
            .context("Failed to find overlay nodes")?;

        let node_count = node_ips.len();

        for (ip, _) in result {
            node_ips.insert(ip);
        }

        log::info!(
            "Found {} overlay nodes in overlay {}",
            node_ips.len() - node_count,
            overlay_id
        );

        if iter.is_none() {
            break;
        }
    }

    Ok(())
}

fn generate_key() -> ed25519_dalek::SecretKey {
    ed25519_dalek::SecretKey::generate(&mut rand::thread_rng())
}

type NodeIps = FxHashSet<AdnlAddressUdp>;
