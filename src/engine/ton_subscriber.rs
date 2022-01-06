use std::sync::Arc;

use anyhow::{Context, Result};
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

    pub async fn start(&self, engine: &ton_indexer::Engine) -> Result<()> {
        let last_key_block = engine
            .load_last_key_block()
            .await
            .context("Failed to load last key block")?;

        let extra = last_key_block
            .block()
            .read_extra()
            .context("Failed to read block extra")?;
        let mc_extra = extra
            .read_custom()
            .context("Failed to read extra custom")?
            .ok_or(TonSubscriberError::NotAMasterChainBlock)?;

        let config = mc_extra.config().context("Key block doesn't have config")?;
        self.metrics
            .update_config_metrics(last_key_block.id().seq_no, config)
            .context("Failed to update config metrics")?;

        Ok(())
    }

    fn update_metrics(&self, block: &BlockStuff) -> Result<()> {
        // Prepare context
        let block_id = block.id();
        let block = block.block();
        let seqno = block_id.seq_no;
        let shard_tag = block_id.shard_id.shard_prefix_with_tag();
        let extra = block.read_extra()?;

        let block_info = block.read_info()?;
        let utime = block_info.gen_utime().0;
        let software_version = block_info.gen_software().map(|v| v.version);

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
        self.metrics.aggregate_block_info(BlockInfo {
            message_count,
            transaction_count,
        });

        if let Some(software_version) = software_version {
            self.metrics
                .handle_software_version(block_id.is_masterchain(), software_version);
        }

        if block_id.is_masterchain() {
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

            self.metrics.update_masterchain_stats(MasterChainStats {
                shard_count,
                seqno,
                utime,
                transaction_count,
            });

            if let Some(config) = mc_extra.config() {
                self.metrics.update_config_metrics(seqno, config)?;
            }
        } else {
            // Update shard chains metrics
            let stats = ShardChainStats {
                shard_tag,
                seqno,
                utime,
                transaction_count,
            };

            if !block_info.after_split() && !block_info.after_merge() {
                // Most common case
                self.metrics.update_shard_chain_stats(stats);
            } else {
                let prev_ids = block_info.read_prev_ids()?;
                self.metrics
                    .force_update_shard_chain_stats(stats, &prev_ids);
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
