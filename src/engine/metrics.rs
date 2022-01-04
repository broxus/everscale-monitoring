use std::sync::atomic::{AtomicU32, Ordering};

use pomfrit::formatter::*;
use tiny_adnl::utils::FxHashMap;

use crate::utils::AverageValueCounter;

#[derive(Default)]
pub struct MetricsState {
    pub blocks_total: AtomicU32,
    pub messages_total: AtomicU32,
    pub transactions_total: AtomicU32,

    pub shard_count: AtomicU32,
    pub shards: parking_lot::RwLock<ShardsMap>,
    pub mc_avg_transaction_count: AverageValueCounter,
}

impl std::fmt::Display for MetricsState {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        const COLLECTION: &str = "collection";
        const SHARD: &str = "shard";

        f.begin_metric("frmulmon_aggregate")
            .label(COLLECTION, "blocks")
            .value(self.blocks_total.load(Ordering::Acquire))?;

        f.begin_metric("frmulmon_aggregate")
            .label(COLLECTION, "messages")
            .value(self.messages_total.load(Ordering::Acquire))?;

        f.begin_metric("frmulmon_aggregate")
            .label(COLLECTION, "transactions")
            .value(self.transactions_total.load(Ordering::Acquire))?;

        f.begin_metric("frmon_mc_shards")
            .value(self.shard_count.load(Ordering::Acquire))?;

        f.begin_metric("frmon_mc_avgtrc")
            .value(self.mc_avg_transaction_count.reset().unwrap_or_default())?;

        for shard in self.shards.read().values() {
            let seqno = shard.seq_no.load(Ordering::Acquire);
            if seqno == 0 {
                continue;
            }

            f.begin_metric("frmon_sc_seqno")
                .label(SHARD, &shard.short_name)
                .value(seqno)?;

            f.begin_metric("frmon_sc_avgtrc")
                .label(SHARD, &shard.short_name)
                .value(shard.avg_transaction_count.reset().unwrap_or_default())?;
        }

        Ok(())
    }
}

pub struct ShardState {
    pub short_name: String,
    pub seq_no: AtomicU32,
    pub avg_transaction_count: AverageValueCounter,
}

impl ShardState {
    pub fn new(id: u64, seq_no: u32, transaction_count: u32) -> Self {
        ShardState {
            short_name: make_short_shard_name(id),
            seq_no: AtomicU32::new(seq_no),
            avg_transaction_count: AverageValueCounter::with_value(transaction_count),
        }
    }

    pub fn update(&self, seq_no: u32, transaction_count: u32) {
        self.seq_no.store(seq_no, Ordering::Release);
        self.avg_transaction_count.push(transaction_count);
    }
}

fn make_short_shard_name(id: u64) -> String {
    format!("{:016x}", id).trim_end_matches('0').to_string()
}

type ShardsMap = FxHashMap<u64, ShardState>;
