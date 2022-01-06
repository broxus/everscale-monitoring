use std::collections::{btree_map, hash_map, BTreeMap};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use pomfrit::formatter::*;
use tiny_adnl::utils::*;
use ton_indexer::EngineMetrics;

use crate::utils::AverageValueCounter;

#[derive(Debug, Copy, Clone)]
pub struct BlockInfo {
    pub message_count: u32,
    pub transaction_count: u32,
}

#[derive(Debug, Copy, Clone)]
pub struct MasterChainStats {
    pub shard_count: u32,
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

#[derive(Default)]
pub struct MetricsState {
    blocks_total: AtomicU32,
    messages_total: AtomicU32,
    transactions_total: AtomicU32,

    shard_count: AtomicU32,
    shards: parking_lot::RwLock<ShardsMap>,
    mc_seq_no: AtomicU32,
    mc_utime: AtomicU32,
    mc_avg_transaction_count: AverageValueCounter,

    mc_software_versions: parking_lot::RwLock<BlockVersions>,
    sc_software_versions: parking_lot::RwLock<BlockVersions>,

    elections_stake_value: parking_lot::RwLock<StakesMap>,
    elections_active_id: AtomicU32,

    config_metrics: parking_lot::Mutex<Option<ConfigMetrics>>,

    engine_metrics: parking_lot::Mutex<Option<Arc<EngineMetrics>>>,
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

    pub fn update_config_metrics(
        &self,
        key_block_seqno: u32,
        config: &ton_block::ConfigParams,
    ) -> Result<()> {
        let mut data = self.config_metrics.lock();
        let data = data.get_or_insert_with(Default::default);
        if key_block_seqno > data.key_block_seqno {
            data.update(config)
        } else {
            Ok(())
        }
    }
}

impl std::fmt::Display for MetricsState {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        const COLLECTION: &str = "collection";
        const SHARD: &str = "shard";
        const SOFTWARE_VERSION: &str = "software_version";
        const ADDRESS: &str = "address";

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

        f.begin_metric("frmon_mc_utime")
            .value(self.mc_utime.load(Ordering::Acquire))?;

        f.begin_metric("frmon_mc_avgtrc")
            .value(self.mc_avg_transaction_count.reset().unwrap_or_default())?;

        for shard in self.shards.read().values() {
            let (seqno, utime) = shard.seqno_and_utime();
            if seqno == 0 {
                continue;
            }

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

        for (address, stake) in &*self.elections_stake_value.read() {
            f.begin_metric("elections_stake_value")
                .label(ADDRESS, address)
                .value(*stake)?;
        }

        f.begin_metric("elections_active_id")
            .value(self.elections_active_id.load(Ordering::Acquire))?;

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

        if let Some(config) = &*self.config_metrics.lock() {
            config.fmt(f)?;
        }

        f.begin_metric("frmon_updated").value(now())?;

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
}

impl ConfigMetrics {
    fn update(&mut self, config: &ton_block::ConfigParams) -> Result<()> {
        let version = config.get_global_version()?;
        let validator_count = config.validators_count()?;
        let stakes_config = match config.config(17)? {
            Some(ton_block::ConfigParamEnum::ConfigParam17(param)) => param,
            _ => return Err(anyhow!("Failed to get config param 17")),
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
        f.begin_metric("config_global_version")
            .value(self.global_version)?;
        f.begin_metric("config_global_capabilities")
            .value(self.global_capabilities)?;

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

        Ok(())
    }
}

struct ShardState {
    short_name: String,
    seqno_and_utime: AtomicU64,
    avg_transaction_count: AverageValueCounter,
}

impl ShardState {
    fn new(stats: ShardChainStats) -> Self {
        ShardState {
            short_name: make_short_shard_name(stats.shard_tag),
            seqno_and_utime: AtomicU64::new(((stats.seqno as u64) << 32) | stats.utime as u64),
            avg_transaction_count: AverageValueCounter::with_value(stats.transaction_count),
        }
    }

    fn update(&self, stats: &ShardChainStats) {
        self.seqno_and_utime.store(
            ((stats.seqno as u64) << 32) | stats.utime as u64,
            Ordering::Release,
        );
        self.avg_transaction_count.push(stats.transaction_count);
    }

    fn seqno_and_utime(&self) -> (u32, u32) {
        let value = self.seqno_and_utime.load(Ordering::Acquire);
        ((value >> 32) as u32, value as u32)
    }
}

fn make_short_shard_name(id: u64) -> String {
    format!("{:016x}", id).trim_end_matches('0').to_string()
}

type ShardsMap = FxHashMap<u64, ShardState>;
type StakesMap = FxHashMap<String, u64>;

type BlockVersions = BTreeMap<u32, AtomicU32>;
