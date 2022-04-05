use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use ton_block::{BinTreeType, Deserializable, HashmapAugType};
use ton_indexer::utils::*;
use ton_indexer::*;

use super::metrics::*;

pub struct TonSubscriber {
    metrics: Arc<MetricsState>,
    elector_address: ton_block::MsgAddressInt,
    elector_address_hash: ton_types::UInt256,
    first_mc_block: AtomicBool,
}

impl TonSubscriber {
    pub fn new(metrics: Arc<MetricsState>) -> Arc<Self> {
        let elector_address_hash = ton_types::UInt256::from_str(
            "3333333333333333333333333333333333333333333333333333333333333333",
        )
        .expect("Shouldn't fail");

        let elector_address = ton_block::MsgAddressInt::with_standart(
            None,
            -1,
            ton_types::SliceData::from(&elector_address_hash),
        )
        .expect("Shouldn't fail");

        Arc::new(Self {
            metrics,
            elector_address,
            elector_address_hash,
            first_mc_block: AtomicBool::new(true),
        })
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

    fn update_metrics(&self, block: &BlockStuff, state: &ShardStateStuff) -> Result<()> {
        // Prepare context
        let block_id = block.id();
        let block = block.block();
        let seqno = block_id.seq_no;
        let shard_tag = block_id.shard_id.shard_prefix_with_tag();
        let is_masterchain = block_id.is_masterchain();
        let extra = block.read_extra()?;

        let block_info = block.read_info()?;
        let utime = block_info.gen_utime().0;
        let software_version = block_info.gen_software().map(|v| v.version);

        // Count transactions and messages
        let mut transaction_count = 0;
        let mut message_count = 0;

        let mut update_elector_state = block_info.key_block();

        let account_blocks = extra.read_account_blocks()?;
        account_blocks.iterate_objects(|block| {
            block
                .transactions()
                .iterate_objects(|ton_block::InRefValue(transaction)| {
                    transaction_count += 1;
                    message_count += transaction.outmsg_cnt as u32;
                    if let Some(in_msg) = transaction.read_in_msg()? {
                        if in_msg.is_inbound_external() {
                            message_count += 1;
                        }

                        update_elector_state |= matches!(
                            in_msg.header(),
                            ton_block::CommonMsgInfo::IntMsgInfo(
                                ton_block::InternalMessageHeader {
                                    value,
                                    dst,
                                    src: ton_block::MsgAddressIntOrNone::Some(src),
                                    ..
                                },
                            ) if in_msg.body().is_some()
                                && value.grams.0 > ELECTOR_UPDATE_THRESHOLD
                                && src.workchain_id() == ton_block::MASTERCHAIN_ID
                                && dst == &self.elector_address
                        );
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
                .handle_software_version(is_masterchain, software_version);
        }

        if is_masterchain {
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

            if update_elector_state || self.first_mc_block.load(Ordering::Acquire) {
                let account = state
                    .state()
                    .read_accounts()
                    .context("Failed to read shard accounts")?
                    .get(&self.elector_address_hash)
                    .context("Failed to get elector account")?;

                if let Some(account) = account {
                    self.metrics
                        .update_elections_state(&account)
                        .context("Failed to update elector state")?;
                }

                self.first_mc_block.store(false, Ordering::Release);
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
    async fn process_block(&self, ctx: ProcessBlockContext<'_>) -> Result<()> {
        let state = match ctx.shard_state_stuff() {
            Some(state) => state,
            None => return Ok(()),
        };

        if let Err(e) = self.update_metrics(ctx.block_stuff(), state) {
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

const ELECTOR_UPDATE_THRESHOLD: u128 = 100_000 * ONE_TON;
const ONE_TON: u128 = 1_000_000_000;

type ShardDescrTreeRef = ton_block::InRefValue<ton_block::BinTree<ton_block::ShardDescr>>;
