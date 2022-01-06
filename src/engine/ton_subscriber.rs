use std::collections::hash_map;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use ton_block::{BinTreeType, Deserializable, HashmapAugType};
use ton_indexer::utils::*;
use ton_indexer::*;

use super::metrics::*;

pub struct TonSubscriber {
    metrics: Arc<MetricsState>,
}

impl TonSubscriber {
    pub fn new(metrics: Arc<MetricsState>) -> Arc<Self> {
        Arc::new(Self { metrics })
    }

    fn update_metrics(&self, block: &BlockStuff) -> Result<()> {
        // Prepare context
        let block_id = block.id();
        let block = block.block();
        let seqno = block_id.seq_no;
        let shard_tag = block_id.shard_id.shard_prefix_with_tag();
        let extra = block.read_extra()?;

        // Count transactions and messages
        let mut transaction_count = 0;
        let mut message_count = 0;

        let account_blocks = extra.read_account_blocks()?;
        account_blocks.iterate_objects(|block| {
            block.transactions().iterate_objects(|transaction| {
                transaction_count += 1;
                message_count += transaction.0.outmsg_cnt as u32;
                if let Some(in_msg) = transaction.0.read_in_msg()? {
                    if in_msg.is_inbound_external() {
                        message_count += 1;
                    }
                }

                Ok(true)
            })?;
            Ok(true)
        })?;

        // Update aggregated metrics
        self.metrics.blocks_total.fetch_add(1, Ordering::Release);
        self.metrics
            .messages_total
            .fetch_add(message_count, Ordering::Release);
        self.metrics
            .transactions_total
            .fetch_add(transaction_count, Ordering::Release);

        if block_id.is_masterchain() {
            // Update masterchain metrics

            // Count shards
            let mc_extra = extra
                .read_custom()?
                .ok_or(TonSubscriberError::NotAMasterChainBlock)?;

            let mut shard_count = 0u32;
            mc_extra.hashes().iterate_slices(|mut slice| {
                let value = ShardDescrTreeRef::construct_from(&mut slice)?.0;
                value.iterate(|_, _| {
                    shard_count += 1;
                    Ok(true)
                })?;

                Ok(true)
            })?;

            self.metrics
                .shard_count
                .swap(shard_count, Ordering::Release);
            self.metrics
                .mc_avg_transaction_count
                .push(transaction_count);
        } else {
            // Update shard chains metrics

            let block_info = block.read_info()?;
            let utime = block_info.gen_utime().0;

            if !block_info.after_split() && !block_info.after_merge() {
                // Most common case
                let shards = self.metrics.shards.read();
                if let Some(shard) = shards.get(&shard_tag) {
                    // Update existing shard metrics
                    shard.update(block_id.seq_no, utime, transaction_count);
                } else {
                    drop(shards);
                    // Force update shard metrics (will only be executed for new shards)
                    match self.metrics.shards.write().entry(shard_tag) {
                        hash_map::Entry::Occupied(entry) => {
                            entry.get().update(seqno, utime, transaction_count)
                        }
                        hash_map::Entry::Vacant(entry) => {
                            entry.insert(ShardState::new(
                                shard_tag,
                                seqno,
                                utime,
                                transaction_count,
                            ));
                        }
                    }
                }
            } else {
                let prev_ids = block_info.read_prev_ids()?;

                // Force update shard metrics
                let mut shards = self.metrics.shards.write();
                match shards.entry(shard_tag) {
                    hash_map::Entry::Occupied(entry) => {
                        entry.get().update(seqno, utime, transaction_count)
                    }
                    hash_map::Entry::Vacant(entry) => {
                        entry.insert(ShardState::new(shard_tag, seqno, utime, transaction_count));
                    }
                }

                // Remove outdated shards
                if prev_ids.len() == 1 {
                    let prev_shard_id = &prev_ids[0].shard_id;
                    let (left, right) = prev_shard_id.split()?;
                    if shards.contains_key(&left.shard_prefix_with_tag())
                        && shards.contains_key(&right.shard_prefix_with_tag())
                    {
                        shards.remove(&prev_shard_id.shard_prefix_with_tag());
                    }
                } else {
                    for block_id in prev_ids {
                        shards.remove(&block_id.shard_id.shard_prefix_with_tag());
                    }
                }
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl ton_indexer::Subscriber for TonSubscriber {
    async fn process_block(
        &self,
        _: BriefBlockMeta,
        block: &BlockStuff,
        _: Option<&BlockProofStuff>,
        _: &ShardStateStuff,
    ) -> Result<()> {
        if let Err(e) = self.update_metrics(block) {
            log::error!("Failed to update metrics: {:?}", e);
        }
        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
enum TonSubscriberError {
    #[error("Given block is not a master block")]
    NotAMasterChainBlock,
}

type ShardDescrTreeRef = ton_block::InRefValue<ton_block::BinTree<ton_block::ShardDescr>>;
