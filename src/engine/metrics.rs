use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use pomfrit::formatter::*;
use tiny_adnl::utils::{now, FxHashMap};
use ton_indexer::EngineMetrics;

use crate::utils::AverageValueCounter;

#[derive(Default)]
pub struct MetricsState {
    pub blocks_total: AtomicU32,
    pub messages_total: AtomicU32,
    pub transactions_total: AtomicU32,

    pub shard_count: AtomicU32,
    pub shards: parking_lot::RwLock<ShardsMap>,
    pub mc_seq_no: AtomicU32,
    pub mc_utime: AtomicU32,
    pub mc_avg_transaction_count: AverageValueCounter,

    pub mc_software_versions: parking_lot::RwLock<BlockVersions>,
    pub sc_software_versions: parking_lot::RwLock<BlockVersions>,

    pub engine_metrics: parking_lot::Mutex<Option<Arc<EngineMetrics>>>,
}

impl std::fmt::Display for MetricsState {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        const COLLECTION: &str = "collection";
        const SHARD: &str = "shard";
        const SOFTWARE_VERSION: &str = "software_version";

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

        if let Some(engine) = &*self.engine_metrics.lock() {
            let mc_seqno = engine.last_mc_block_seqno.load(Ordering::Acquire);
            if mc_seqno > 0 {
                f.begin_metric("mc_time_diff")
                    .value(engine.mc_time_diff.load(Ordering::Acquire))?;
            }

            let mc_shard_seqno = engine
                .last_shard_client_mc_block_seqno
                .load(Ordering::Acquire);
            if mc_shard_seqno > 0 {
                f.begin_metric("mc_shard_time_diff")
                    .value(engine.shard_client_time_diff.load(Ordering::Acquire))?;
            }
        }

        f.begin_metric("frmon_updated").value(now())?;

        Ok(())
    }
}

pub struct ShardState {
    pub short_name: String,
    pub seqno_and_utime: AtomicU64,
    pub avg_transaction_count: AverageValueCounter,
}

impl ShardState {
    pub fn new(id: u64, seqno: u32, utime: u32, transaction_count: u32) -> Self {
        ShardState {
            short_name: make_short_shard_name(id),
            seqno_and_utime: AtomicU64::new(((seqno as u64) << 32) | utime as u64),
            avg_transaction_count: AverageValueCounter::with_value(transaction_count),
        }
    }

    pub fn update(&self, seqno: u32, utime: u32, transaction_count: u32) {
        self.seqno_and_utime
            .store(((seqno as u64) << 32) | utime as u64, Ordering::Release);
        self.avg_transaction_count.push(transaction_count);
    }

    pub fn seqno_and_utime(&self) -> (u32, u32) {
        let value = self.seqno_and_utime.load(Ordering::Acquire);
        ((value >> 32) as u32, value as u32)
    }
}

fn make_short_shard_name(id: u64) -> String {
    format!("{:016x}", id).trim_end_matches('0').to_string()
}

type ShardsMap = FxHashMap<u64, ShardState>;
type BlockVersions = FxHashMap<u32, AtomicU32>;
