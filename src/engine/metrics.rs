use std::collections::{btree_map, hash_map, BTreeMap};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use nekoton_abi::*;
use once_cell::race::OnceBox;
use pomfrit::formatter::*;
use tiny_adnl::utils::*;
use ton_indexer::EngineMetrics;
use ton_types::UInt256;

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

    elections_state: parking_lot::RwLock<ElectionsState>,
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

    pub fn update_elections_state(&self, elector_account: &ton_block::ShardAccount) -> Result<()> {
        self.elections_state.write().update(elector_account)
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
            .fetch_and(Self::DIRTY_MASK, Ordering::AcqRel)
            & Self::DIRTY_MASK;

        if value & Self::DIRTY_FLAG != 0 {
            Some(((value >> 32) as u32, value as u32))
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

        log::info!("Updating elector state");
        let account = match elector_account.read_account()? {
            ton_block::Account::Account(account) => account,
            ton_block::Account::AccountNone => return Ok(()),
        };

        let state = match account.storage.state {
            ton_block::AccountState::AccountActive { state_init, .. } => state_init,
            _ => return Ok(()),
        };

        let data = match state.data {
            Some(data) => data,
            None => return Ok(()),
        };

        let current_election: MaybeRef<CurrentElectionData> = ton_abi::TokenValue::decode_params(
            elector_state_params(),
            data.into(),
            &ton_abi::contract::ABI_VERSION_2_1,
            true,
        )
        .context("Failed to decode elector state data")?
        .unpack_first()
        .context("Failed to parse decoded elector state data")?;

        if let Some(election) = current_election.0 {
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

fn elector_state_params() -> &'static [ton_abi::Param] {
    static ABI: OnceBox<Vec<ton_abi::Param>> = OnceBox::new();
    ABI.get_or_init(|| {
        Box::new(vec![ton_abi::Param::new(
            "current_election",
            MaybeRef::<CurrentElectionData>::param_type(),
        )])
    })
}

#[derive(Debug, UnpackAbi, KnownParamType)]
pub struct CurrentElectionData {
    #[abi(uint32)]
    pub elect_at: u32,
    #[abi(uint32)]
    pub elect_close: u32,
    #[abi(gram)]
    pub min_stake: u128,
    #[abi(gram)]
    pub total_stake: u128,
    #[abi(
        unpack_with = "unpack_map_uint256_tuple",
        param_type_with = "members_param_type"
    )]
    pub members: BTreeMap<UInt256, ElectionMember>,
    #[abi(bool)]
    pub failed: bool,
    #[abi(bool)]
    pub finished: bool,
}

#[derive(Debug, UnpackAbi, KnownParamType)]
pub struct ElectionMember {
    #[abi(gram)]
    pub msg_value: u64,
    #[abi(uint32)]
    pub created_at: u32,
    #[abi(uint32)]
    pub max_factor: u32,
    #[abi(with = "uint256_bytes")]
    pub src_addr: UInt256,
    #[abi(with = "uint256_bytes")]
    pub adnl_addr: UInt256,
}

pub fn unpack_map_uint256_tuple<V>(
    value: &ton_abi::TokenValue,
) -> UnpackerResult<BTreeMap<UInt256, V>>
where
    ton_abi::TokenValue: UnpackAbi<V>,
{
    match value {
        ton_abi::TokenValue::Map(ton_abi::ParamType::Uint(256), _, values) => {
            let mut map = BTreeMap::<UInt256, V>::new();
            for (key, value) in values {
                let key = key
                    .parse::<UInt256>()
                    .map_err(|_| UnpackerError::InvalidAbi)?;
                let value: V = value.to_owned().unpack()?;
                map.insert(key, value);
            }
            Ok(map)
        }
        _ => Err(UnpackerError::InvalidAbi),
    }
}

fn members_param_type() -> ton_abi::ParamType {
    ton_abi::ParamType::Map(
        Box::new(UInt256::param_type()),
        Box::new(ElectionMember::param_type()),
    )
}

fn make_short_shard_name(id: u64) -> String {
    format!("{:016x}", id).trim_end_matches('0').to_string()
}

type ShardsMap = FxHashMap<u64, ShardState>;
type StakesMap = FxHashMap<String, u64>;

type BlockVersions = BTreeMap<u32, AtomicU32>;
