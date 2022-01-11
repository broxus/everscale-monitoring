use std::net::SocketAddrV4;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures::StreamExt;
use global_config::*;
use tiny_adnl::utils::{AdnlAddressUdp, AdnlNodeIdFull, AdnlNodeIdShort, FxHashMap};
use tiny_adnl::*;

pub async fn search_nodes(address: SocketAddrV4, global_config: GlobalConfig) -> Result<NodesMap> {
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

    let overlay = OverlayNode::new(adnl.clone(), global_config.zero_state.file_hash.into(), 1)
        .context("Failed to create overlay node")?;

    adnl.start(vec![dht.clone(), overlay.clone()])
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
    let mut nodes = FxHashMap::default();
    scan_overlay(&dht, &overlay, -1, &mut nodes)
        .await
        .context("Failed to scan overlay -1")?;
    scan_overlay(&dht, &overlay, 0, &mut nodes)
        .await
        .context("Failed to scan overlay 0")?;

    // Done
    Ok(nodes)
}

async fn scan_overlay(
    dht: &Arc<DhtNode>,
    overlay: &Arc<OverlayNode>,
    workchain: i32,
    nodes: &mut NodesMap,
) -> Result<()> {
    let overlay_id = overlay.compute_overlay_short_id(workchain, 0x8000000000000000u64 as i64)?;
    log::info!("Scanning overlay {}", overlay_id);

    let mut iter = None;
    loop {
        let result = dht
            .find_overlay_nodes(&overlay_id, &mut iter)
            .await
            .context("Failed to find overlay nodes")?;

        let node_count = nodes.len();

        for (ip, node) in result {
            let peer_id = AdnlNodeIdFull::try_from(&node.id)
                .context("Failed to get peer full id")?
                .compute_short_id()
                .context("Failed to compute peer short id")?;
            nodes.insert(peer_id, ip);
        }

        log::info!(
            "Found {} overlay nodes in overlay {}",
            nodes.len() - node_count,
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

type NodesMap = FxHashMap<AdnlNodeIdShort, AdnlAddressUdp>;
