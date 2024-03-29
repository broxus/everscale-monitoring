use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;

use anyhow::{Context, Result};
use everscale_network::{adnl, dht, overlay, rldp};
use rand::Rng;
use serde::{Deserialize, Serialize};

/// Main application config
#[derive(Serialize, Deserialize)]
pub struct AppConfig {
    /// TON node settings
    pub node_settings: NodeConfig,

    pub metrics_settings: pomfrit::Config,
}

/// TON node settings
#[derive(Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeConfig {
    /// Node public ip. Automatically determines if None
    pub adnl_public_ip: Option<Ipv4Addr>,

    /// Node port. Default: 30303
    pub adnl_port: u16,

    /// Path to the DB directory. Default: `./db`
    pub db_path: PathBuf,

    /// Path to the ADNL keys. Default: `./adnl-keys.json`.
    /// NOTE: generates new keys if specified path doesn't exist
    pub temp_keys_path: PathBuf,

    /// Persistent state keeper configuration.
    pub persistent_state_options: ton_indexer::PersistentStateOptions,

    /// Internal DB options.
    pub db_options: ton_indexer::DbOptions,

    /// Archives map queue. Default: 16
    pub parallel_archive_downloads: usize,

    /// Whether old shard states will be removed every 10 minutes
    pub states_gc_enabled: bool,

    /// Whether old blocks will be removed on each new key block
    pub blocks_gc_enabled: bool,

    /// Archives GC and uploader options
    pub archive_options: Option<ton_indexer::ArchiveOptions>,

    /// Start from the specified seqno
    pub start_from: Option<u32>,

    pub adnl_options: adnl::NodeOptions,
    pub rldp_options: rldp::NodeOptions,
    pub dht_options: dht::NodeOptions,
    pub overlay_shard_options: overlay::OverlayOptions,
    pub neighbours_options: ton_indexer::NeighboursOptions,
}

impl NodeConfig {
    pub async fn build_indexer_config(mut self) -> Result<ton_indexer::NodeConfig> {
        // Determine public ip
        let ip_address = broxus_util::resolve_public_ip(self.adnl_public_ip).await?;
        tracing::info!(?ip_address, "using public ip");

        // Generate temp keys
        let adnl_keys = ton_indexer::NodeKeys::load(self.temp_keys_path, false)
            .context("Failed to load temp keys")?;

        // Prepare DB folder
        std::fs::create_dir_all(&self.db_path)?;

        let old_blocks_policy = match self.start_from {
            None => ton_indexer::OldBlocksPolicy::Ignore,
            Some(from_seqno) => ton_indexer::OldBlocksPolicy::Sync { from_seqno },
        };

        self.rldp_options.force_compression = true;
        self.overlay_shard_options.force_compression = true;

        // Done
        Ok(ton_indexer::NodeConfig {
            ip_address: SocketAddrV4::new(ip_address, self.adnl_port),
            adnl_keys,
            rocks_db_path: self.db_path.join("rocksdb"),
            file_db_path: self.db_path.join("files"),
            state_gc_options: self.states_gc_enabled.then(|| ton_indexer::StateGcOptions {
                offset_sec: rand::thread_rng().gen_range(0..3600),
                interval_sec: 3600,
            }),
            blocks_gc_options: self
                .blocks_gc_enabled
                .then(|| ton_indexer::BlocksGcOptions {
                    kind: ton_indexer::BlocksGcKind::BeforePreviousKeyBlock,
                    enable_for_sync: true,
                    ..Default::default()
                }),
            shard_state_cache_options: None,
            persistent_state_options: self.persistent_state_options,
            db_options: self.db_options,
            archive_options: self.archive_options,
            sync_options: ton_indexer::SyncOptions {
                old_blocks_policy,
                parallel_archive_downloads: self.parallel_archive_downloads,
                ..Default::default()
            },
            adnl_options: self.adnl_options,
            rldp_options: self.rldp_options,
            dht_options: self.dht_options,
            overlay_shard_options: self.overlay_shard_options,
            neighbours_options: self.neighbours_options,
        })
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            adnl_public_ip: None,
            adnl_port: 30303,
            db_path: "db".into(),
            temp_keys_path: "adnl-keys.json".into(),
            persistent_state_options: Default::default(),
            db_options: Default::default(),
            parallel_archive_downloads: 16,
            states_gc_enabled: true,
            blocks_gc_enabled: true,
            archive_options: Some(Default::default()),
            start_from: None,
            adnl_options: Default::default(),
            rldp_options: Default::default(),
            dht_options: Default::default(),
            overlay_shard_options: Default::default(),
            neighbours_options: Default::default(),
        }
    }
}
