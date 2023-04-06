use std::collections::{btree_map, hash_map, BTreeMap};
use std::net::SocketAddrV4;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use broxus_util::now;
use pomfrit::formatter::*;
use rustc_hash::FxHashMap;
use ton_indexer::EngineMetrics;

use super::elector;
use crate::utils::AverageValueCounter;

#[derive(Debug, Copy, Clone)]
pub struct BlockInfo {
    pub message_count: u32,
    pub transaction_count: u32,
}

#[derive(Debug, Copy, Clone)]
pub struct MasterChainStats {
    pub shard_count: usize,
    pub seqno: u32,
    pub utime: u32,
    pub transaction_count: u32,
}

#[derive(Debug, Copy, Clone)]
pub struct ShardChainStats {
    pub shard_tag: u64,
    pub seqno: u32,
    pub utime: u32,
    pub transaction_count: u32,
}

#[derive(Debug, Clone)]
pub struct PrivateOverlayStats {
    pub overlay_id: [u8; 32],
    pub workchain: i32,
    pub shard_tag: u64,
    pub catchain_seqno: u32,
    pub validators: Vec<PrivateOverlayEntry>,
}

#[derive(Debug, Clone)]
pub struct PrivateOverlayEntry {
    pub public_key: [u8; 32],
}

struct StoredPrivateOverlayStats {
    overlay_id: String,
    workchain: i32,
    shard_tag: String,
    catchain_seqno: u32,
    validators: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ValidatorInfo {
    pub public_key: [u8; 32],
    pub adnl: [u8; 32],
    pub wallet: [u8; 32],
    pub stake: u64,
}

struct StoredValidatorInfo {
    public_key: String,
    adnl: String,
    wallet: String,
    stake: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct PersistentStateInfo {
    pub seqno: u32,
    pub gen_utime: u32,
}

#[derive(Default)]
pub struct MetricsState {
    blocks_total: AtomicU32,
    messages_total: AtomicU32,
    transactions_total: AtomicU32,

    shard_count: AtomicUsize,
    shards: parking_lot::RwLock<ShardsMap>,
    overlay_metrics: parking_lot::RwLock<Vec<StoredPrivateOverlayStats>>,
    mc_seq_no: AtomicU32,
    mc_utime: AtomicU32,
    mc_avg_transaction_count: AverageValueCounter,

    mc_software_versions: parking_lot::RwLock<BlockVersions>,
    sc_software_versions: parking_lot::RwLock<BlockVersions>,

    elections_state: parking_lot::RwLock<ElectionsState>,
    validators: parking_lot::RwLock<Vec<StoredValidatorInfo>>,
    validator_ips: parking_lot::RwLock<FxHashMap<[u8; 32], String>>,
    config_metrics: parking_lot::Mutex<Option<ConfigMetrics>>,
    engine_metrics: parking_lot::Mutex<Option<Arc<EngineMetrics>>>,
    persistent_state: parking_lot::Mutex<Option<PersistentStateInfo>>,
}

impl MetricsState {
    pub fn set_engine_metrics(&self, engine_metrics: &Arc<EngineMetrics>) {
        *self.engine_metrics.lock() = Some(engine_metrics.clone());
    }

    pub fn aggregate_block_info(&self, info: BlockInfo) {
        self.blocks_total.fetch_add(1, Ordering::Release);
        self.messages_total
            .fetch_add(info.message_count, Ordering::Release);
        self.transactions_total
            .fetch_add(info.transaction_count, Ordering::Release);
    }

    pub fn handle_software_version(&self, is_masterchain: bool, version: u32) {
        let versions_map = if is_masterchain {
            &self.mc_software_versions
        } else {
            &self.sc_software_versions
        };

        let versions = versions_map.read();
        if let Some(count) = versions.get(&version) {
            count.fetch_add(1, Ordering::Release);
        } else {
            drop(versions);
            match versions_map.write().entry(version) {
                btree_map::Entry::Occupied(entry) => {
                    entry.get().fetch_add(1, Ordering::Release);
                }
                btree_map::Entry::Vacant(entry) => {
                    entry.insert(AtomicU32::new(1));
                }
            }
        }
    }
    pub fn update_private_overlays(&self, overlays: Vec<PrivateOverlayStats>) {
        let overlays = overlays
            .into_iter()
            .map(|x| StoredPrivateOverlayStats {
                overlay_id: hex::encode(x.overlay_id),
                workchain: x.workchain,
                shard_tag: make_short_shard_name(x.shard_tag),
                catchain_seqno: x.catchain_seqno,
                validators: x
                    .validators
                    .into_iter()
                    .map(|v| hex::encode(v.public_key))
                    .collect(),
            })
            .collect();
        *self.overlay_metrics.write() = overlays;
    }

    pub fn update_masterchain_stats(&self, stats: MasterChainStats) {
        self.shard_count.swap(stats.shard_count, Ordering::Release);
        self.mc_seq_no.store(stats.seqno, Ordering::Release);
        self.mc_utime.store(stats.utime, Ordering::Release);
        self.mc_avg_transaction_count.push(stats.transaction_count);
    }

    pub fn update_shard_chain_stats(&self, stats: ShardChainStats) {
        let shards = self.shards.read();
        if let Some(shard) = shards.get(&stats.shard_tag) {
            shard.update(&stats);
        } else {
            drop(shards);
            match self.shards.write().entry(stats.shard_tag) {
                hash_map::Entry::Occupied(entry) => entry.get().update(&stats),
                hash_map::Entry::Vacant(entry) => {
                    entry.insert(ShardState::new(stats));
                }
            }
        }
    }

    pub fn force_update_shard_chain_stats(
        &self,
        stats: ShardChainStats,
        prev_ids: &[ton_block::BlockIdExt],
    ) {
        // Force update shard metrics
        let mut shards = self.shards.write();
        match shards.entry(stats.shard_tag) {
            hash_map::Entry::Occupied(entry) => entry.get().update(&stats),
            hash_map::Entry::Vacant(entry) => {
                entry.insert(ShardState::new(stats));
            }
        }

        // Remove outdated shards
        match prev_ids {
            [prev] => {
                let prev_shard_id = &prev.shard_id;
                if let Ok((left, right)) = prev_shard_id.split() {
                    if shards.contains_key(&left.shard_prefix_with_tag())
                        && shards.contains_key(&right.shard_prefix_with_tag())
                    {
                        shards.remove(&prev_shard_id.shard_prefix_with_tag());
                    }
                }
            }
            _ => {
                for block_id in prev_ids {
                    shards.remove(&block_id.shard_id.shard_prefix_with_tag());
                }
            }
        }
    }

    pub fn update_elections_state(&self, elector_account: &ton_block::ShardAccount) -> Result<()> {
        self.elections_state.write().update(elector_account)
    }

    pub fn update_validators(&self, validators: Vec<ValidatorInfo>) {
        let validators = validators
            .into_iter()
            .map(|v| StoredValidatorInfo {
                public_key: hex::encode(v.public_key),
                adnl: hex::encode(v.adnl),
                wallet: format!("-1:{}", hex::encode(v.wallet)),
                stake: v.stake,
            })
            .collect();
        *self.validators.write() = validators;
    }

    pub fn update_validator_ip(&self, public_key: &[u8; 32], ip: SocketAddrV4) {
        self.validator_ips
            .write()
            .insert(*public_key, ip.to_string());
    }

    pub fn clear_validator_ips(&self) {
        self.validator_ips.write().clear();
    }

    pub fn update_config_metrics(
        &self,
        key_block_seqno: u32,
        config: &ton_block::ConfigParams,
    ) -> Result<()> {
        let mut data = self.config_metrics.lock();
        let data = data.get_or_insert_with(Default::default);
        if key_block_seqno > data.key_block_seqno {
            data.key_block_seqno = key_block_seqno;
            data.update(config)
        } else {
            Ok(())
        }
    }

    pub fn update_persistent_state(&self, info: PersistentStateInfo) {
        *self.persistent_state.lock() = Some(info);
    }
}

impl std::fmt::Display for MetricsState {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        const COLLECTION: &str = "collection";
        const WORKCHAIN: &str = "workchain";
        const SHARD: &str = "shard";
        const SOFTWARE_VERSION: &str = "software_version";
        const ADDRESS: &str = "address";
        const OVERLAY_ID: &str = "overlay_id";
        const CATCHAIN_SEQNO: &str = "catchain_seqno";
        const PUBKEY: &str = "pubkey";
        const ADNL: &str = "adnl";
        const SOCKET_ADDR: &str = "socket_addr";

        let mc_utime = self.mc_utime.load(Ordering::Acquire);
        if mc_utime == 0 {
            return Ok(());
        }

        f.begin_metric("frmon_aggregate")
            .label(COLLECTION, "blocks")
            .value(self.blocks_total.load(Ordering::Acquire))?;

        f.begin_metric("frmon_aggregate")
            .label(COLLECTION, "messages")
            .value(self.messages_total.load(Ordering::Acquire))?;

        f.begin_metric("frmon_aggregate")
            .label(COLLECTION, "transactions")
            .value(self.transactions_total.load(Ordering::Acquire))?;

        f.begin_metric("frmon_mc_shards")
            .value(self.shard_count.load(Ordering::Acquire))?;

        f.begin_metric("frmon_mc_seqno")
            .value(self.mc_seq_no.load(Ordering::Acquire))?;

        f.begin_metric("frmon_mc_utime").value(mc_utime)?;

        f.begin_metric("frmon_mc_avgtrc")
            .value(self.mc_avg_transaction_count.reset().unwrap_or_default())?;

        for shard in self.shards.read().values() {
            if let Some((seqno, utime)) = shard.load_seqno_and_utime() {
                f.begin_metric("frmon_sc_seqno")
                    .label(SHARD, &shard.short_name)
                    .value(seqno)?;

                f.begin_metric("frmon_sc_utime")
                    .label(SHARD, &shard.short_name)
                    .value(utime)?;

                f.begin_metric("frmon_sc_avgtrc")
                    .label(SHARD, &shard.short_name)
                    .value(shard.avg_transaction_count.reset().unwrap_or_default())?;
            }
        }

        for overlay in self.overlay_metrics.read().iter() {
            f.begin_metric("overlay")
                .label(OVERLAY_ID, &overlay.overlay_id)
                .label(WORKCHAIN, overlay.workchain)
                .label(SHARD, &overlay.shard_tag)
                .label(CATCHAIN_SEQNO, overlay.catchain_seqno)
                .value(1)?;

            for pubkey in &overlay.validators {
                f.begin_metric("overlay_entry")
                    .label(OVERLAY_ID, &overlay.overlay_id)
                    .label(PUBKEY, pubkey)
                    .value(1)?;
            }
        }

        for (version, count) in &*self.mc_software_versions.read() {
            let count = count.swap(0, Ordering::AcqRel);
            if count > 0 {
                f.begin_metric("frmon_mc_software_version")
                    .label(SOFTWARE_VERSION, version)
                    .value(count)?;
            }
        }

        for (version, count) in &*self.sc_software_versions.read() {
            let count = count.swap(0, Ordering::AcqRel);
            if count > 0 {
                f.begin_metric("frmon_sc_software_version")
                    .label(SOFTWARE_VERSION, version)
                    .value(count)?;
            }
        }

        {
            let elections = self.elections_state.read();
            if let Some(stakes) = &elections.stakes {
                for (address, stake) in stakes {
                    f.begin_metric("elections_stake_value")
                        .label(ADDRESS, address)
                        .value(*stake)?;
                }

                f.begin_metric("elections_elect_at")
                    .value(elections.elect_at)?;

                f.begin_metric("elections_elect_close")
                    .value(elections.elect_close)?;
            }
        }

        for validator in self.validators.read().iter() {
            f.begin_metric("validator")
                .label(PUBKEY, &validator.public_key)
                .label(ADNL, &validator.adnl)
                .label(ADDRESS, &validator.wallet)
                .value(validator.stake)?;
        }

        for (pubkey, ip) in self.validator_ips.read().iter() {
            f.begin_metric("validator_ip")
                .label(PUBKEY, hex::encode(pubkey))
                .label(SOCKET_ADDR, ip)
                .value(1)?;
        }

        if let Some(engine) = &*self.engine_metrics.lock() {
            f.begin_metric("frmon_mc_time_diff")
                .value(engine.mc_time_diff.load(Ordering::Acquire))?;

            f.begin_metric("frmon_mc_shard_seqno").value(
                engine
                    .last_shard_client_mc_block_seqno
                    .load(Ordering::Acquire),
            )?;

            f.begin_metric("frmon_mc_shard_time_diff")
                .value(engine.shard_client_time_diff.load(Ordering::Acquire))?;
        }

        if let Some(persistent_state) = &*self.persistent_state.lock() {
            f.begin_metric("persistent_state_seqno")
                .value(persistent_state.seqno)?;
            f.begin_metric("persistent_state_utime")
                .value(persistent_state.gen_utime)?;
            f.begin_metric("persistent_state_time_diff")
                .value(now().saturating_sub(persistent_state.gen_utime))?;
        }

        if let Some(config) = &*self.config_metrics.lock() {
            config.fmt(f)?;
        }

        f.begin_metric("frmon_updated")
            .label("exporter_version", crate::VERSION)
            .value(now())?;

        Ok(())
    }
}

#[derive(Default)]
struct ConfigMetrics {
    key_block_seqno: u32,

    global_version: u32,
    global_capabilities: u64,

    max_validators: u32,
    max_main_validators: u32,
    min_validators: u32,

    min_stake: u64,
    max_stake: u64,
    min_total_stake: u64,
    max_stake_factor: u32,

    min_split: u8,
    max_split: u8,
}

impl ConfigMetrics {
    fn update(&mut self, config: &ton_block::ConfigParams) -> Result<()> {
        let version = config.get_global_version()?;
        let validator_count = config.validators_count()?;
        let stakes_config = match config.config(17)? {
            Some(ton_block::ConfigParamEnum::ConfigParam17(param)) => param,
            _ => return Err(anyhow!("Failed to get config param 17")),
        };

        if let Some(workchain) = config.workchains()?.get(&0)? {
            self.min_split = workchain.min_split();
            self.max_split = workchain.max_split();
        };

        self.global_version = version.version;
        self.global_capabilities = version.capabilities;

        self.max_validators = validator_count.max_validators.0;
        self.max_main_validators = validator_count.max_main_validators.0;
        self.min_validators = validator_count.min_validators.0;

        self.min_stake = stakes_config.min_stake.0.try_into().unwrap_or(u64::MAX);
        self.max_stake = stakes_config.max_stake.0.try_into().unwrap_or(u64::MAX);
        self.min_total_stake = stakes_config
            .min_total_stake
            .0
            .try_into()
            .unwrap_or(u64::MAX);
        self.max_stake_factor = stakes_config.max_stake_factor;

        Ok(())
    }
}

impl std::fmt::Display for ConfigMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.begin_metric("key_block_seqno")
            .value(self.key_block_seqno)?;
        f.begin_metric("config_global_version")
            .value(self.global_version)?;
        f.begin_metric("config_global_capabilities")
            .value(self.global_capabilities)?;

        macro_rules! export_capabilities {
            ($($bit:literal => $name:literal),*$(,)?) => {
                $(
                    f.begin_metric("config_global_capability")
                        .label("capability", $name)
                        .label("bit", ($bit as u32).trailing_zeros())
                        .value(u8::from(self.global_capabilities & $bit != 0))?;
                )*
            };
        }

        export_capabilities! {
            0x00000001 => "IhrEnabled",
            0x00000002 => "CreateStatsEnabled",
            0x00000004 => "BounceMsgBody",
            0x00000008 => "ReportVersion",
            0x00000010 => "SplitMergeTransactions",
            0x00000020 => "ShortDequeue",
            0x00000040 => "MbppEnabled",
            0x00000080 => "FastStorageStat",
            0x00000100 => "InitCodeHash",
            0x00000200 => "OffHypercube",
            0x00000400 => "Mycode",
            0x00000800 => "SetLibCode",
            0x00001000 => "FixTupleIndexBug",
            0x00002000 => "Remp",
            0x00004000 => "Delections",
            0x00010000 => "FullBodyInBounced",
            0x00020000 => "StorageFeeToTvm",
            0x00040000 => "Copyleft",
            0x00080000 => "IndexAccounts",
            0x00200000 => "TvmBugfixes2022",
            0x00400000 => "Workchains",
            0x00800000 => "StcontNewFormat",
            0x01000000 => "FastStorageStatBugfix",
            0x02000000 => "ResolveMerkleCell",
            0x04000000 => "SignatureWithId",
            0x08000000 => "BounceAfterFailedAction",
            0x10000000 => "Groth16",
            0x20000000 => "FeeInGasUnits",
        }

        f.begin_metric("config_max_validators")
            .value(self.max_validators)?;
        f.begin_metric("config_max_main_validators")
            .value(self.max_main_validators)?;
        f.begin_metric("config_min_validators")
            .value(self.min_validators)?;

        f.begin_metric("config_min_stake").value(self.min_stake)?;
        f.begin_metric("config_max_stake").value(self.max_stake)?;
        f.begin_metric("config_min_total_stake")
            .value(self.min_total_stake)?;
        f.begin_metric("config_max_stake_factor")
            .value(self.max_stake_factor)?;

        f.begin_metric("workchain_min_split")
            .label("workchain", 0)
            .value(self.min_split)?;
        f.begin_metric("workchain_max_split")
            .label("workchain", 0)
            .value(self.max_split)?;

        Ok(())
    }
}

struct ShardState {
    short_name: String,
    seqno_and_utime: AtomicU64,
    avg_transaction_count: AverageValueCounter,
}

impl ShardState {
    const DIRTY_FLAG: u64 = 0x0000_0000_8000_0000;
    const DIRTY_MASK: u64 = !Self::DIRTY_FLAG;

    fn new(stats: ShardChainStats) -> Self {
        ShardState {
            short_name: make_short_shard_name(stats.shard_tag),
            seqno_and_utime: AtomicU64::new(
                (((stats.seqno as u64) << 32) | stats.utime as u64) | Self::DIRTY_FLAG,
            ),
            avg_transaction_count: AverageValueCounter::with_value(stats.transaction_count),
        }
    }

    fn update(&self, stats: &ShardChainStats) {
        self.seqno_and_utime.store(
            (((stats.seqno as u64) << 32) | stats.utime as u64) | Self::DIRTY_FLAG,
            Ordering::Release,
        );
        self.avg_transaction_count.push(stats.transaction_count);
    }

    fn load_seqno_and_utime(&self) -> Option<(u32, u32)> {
        let value = self
            .seqno_and_utime
            .fetch_and(Self::DIRTY_MASK, Ordering::AcqRel);

        if value & Self::DIRTY_FLAG != 0 {
            Some(((value >> 32) as u32, (value & Self::DIRTY_MASK) as u32))
        } else {
            None
        }
    }
}

#[derive(Default)]
struct ElectionsState {
    last_transaction_lt: u64,
    stakes: Option<StakesMap>,
    elect_at: u32,
    elect_close: u32,
    min_stake: u128,
    total_stake: u128,
}

impl ElectionsState {
    fn update(&mut self, elector_account: &ton_block::ShardAccount) -> Result<()> {
        if elector_account.last_trans_lt() <= self.last_transaction_lt {
            return Ok(());
        }

        tracing::info!("updating elector state");
        if let Some(election) = elector::parse_current_election(elector_account)? {
            self.stakes = Some(
                election
                    .members
                    .into_values()
                    .map(|item| (format!("-1:{:x}", item.src_addr), item.msg_value))
                    .collect(),
            );
            self.elect_at = election.elect_at;
            self.elect_close = election.elect_close;
            self.min_stake = election.min_stake;
            self.total_stake = election.total_stake;
        } else {
            self.stakes = None;
        }

        Ok(())
    }
}

fn make_short_shard_name(id: u64) -> String {
    format!("{:016x}", id).trim_end_matches('0').to_string()
}

type ShardsMap = FxHashMap<u64, ShardState>;
type StakesMap = FxHashMap<String, u64>;

type BlockVersions = BTreeMap<u32, AtomicU32>;
