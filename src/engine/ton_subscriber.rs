use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::memory_storage::MemoryStorage;
use anyhow::{anyhow, Context, Result};
use everscale_network::adnl::NodeIdShort;
use everscale_network::dht;
use everscale_network::overlay::IdFull;
use futures::stream::FuturesOrdered;
use futures::stream::StreamExt;
use tokio::sync::Mutex;
use ton_block::{
    BinTreeType, ConfigParamEnum, ConfigParams, Deserializable, GetRepresentationHash,
    HashmapAugType, McStateExtra, ShardAccount, ShardIdent, ValidatorSet,
};
use ton_indexer::utils::*;
use ton_indexer::*;
use ton_types::UInt256;

use crate::tl_models::*;

use super::metrics::*;

pub struct TonSubscriber {
    metrics: Arc<MetricsState>,
    last_key_block_extra: Mutex<Arc<Option<ConfigParams>>>,
    elector_address: ton_block::MsgAddressInt,
    elector_address_hash: ton_types::UInt256,
    first_mc_block: AtomicBool,
    dht: Arc<dht::Node>,
    memory_storage: Arc<MemoryStorage>,
}

impl TonSubscriber {
    pub fn new(
        metrics: Arc<MetricsState>,
        dht: Arc<dht::Node>,
        memory_storage: Arc<MemoryStorage>,
    ) -> Arc<Self> {
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

        let extra = Mutex::new(Arc::new(None));
        Arc::new(Self {
            metrics,
            last_key_block_extra: extra,
            elector_address,
            elector_address_hash,
            first_mc_block: AtomicBool::new(true),
            dht,
            memory_storage,
        })
    }

    pub async fn start(&self, engine: &Engine) -> Result<()> {
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
        self.update_last_key_block_config(config).await;

        self.metrics
            .update_config_metrics(last_key_block.id().seq_no, config)
            .context("Failed to update config metrics")?;

        Ok(())
    }

    async fn update_last_key_block_config(&self, config: &ConfigParams) {
        let mut extra = self.last_key_block_extra.lock().await;
        *extra = Arc::new(Some(config.clone()));
    }

    async fn update_metrics(&self, block: &BlockStuff, state: &ShardStateStuff) -> Result<()> {
        // Prepare context
        let block_id = block.id();
        let block = block.block();
        let seqno = block_id.seq_no;
        let shard_tag = block_id.shard_id.shard_prefix_with_tag();
        let is_masterchain = block_id.is_masterchain();
        let extra = block.read_extra()?;
        let info = block.read_info()?;

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
            let catchain_seqno = info.gen_catchain_seqno();

            let mc_extra = extra
                .read_custom()?
                .ok_or(TonSubscriberError::NotAMasterChainBlock)?;

            let mut shard_count = 0u32;
            let mut shards: Vec<ShardIdent> = Vec::new();
            shards.push(ShardIdent::masterchain());

            mc_extra.hashes().iterate_shards(|shard, _| {
                shards.push(shard);
                Ok(true)
            })?;

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
                self.update_last_key_block_config(config).await;
            }

            let elector_account = state
                .state()
                .read_accounts()
                .context("Failed to read shard accounts")?
                .get(&self.elector_address_hash)
                .context("Failed to get elector account")?;

            if update_elector_state || self.first_mc_block.load(Ordering::Acquire) {
                if let Some(account) = elector_account.as_ref() {
                    self.metrics
                        .update_elections_state(account)
                        .context("Failed to update elector state")?;
                }
                self.first_mc_block.store(false, Ordering::Release);
            }

            let current_config = self.last_key_block_extra.lock().await;
            let current_config = current_config.as_ref();
            if let Some(config) = current_config {
                self.metrics.update_config_metrics(seqno, config)?;

                let catchain_config = config
                    .catchain_config()
                    .context("Failed to get catchain config")?;
                let validator_set = config
                    .validator_set()
                    .context("Failed to get full validator set")?;

                let mut private_overlays: Vec<PrivateOverlayStats> = Vec::new();

                let shard_state_extra = state.shard_state_extra()?;
                for shard in shards {
                    let (val_subset, _) = validator_set.calc_subset(
                        &catchain_config,
                        shard.shard_prefix_with_tag(),
                        shard.workchain_id(),
                        catchain_seqno,
                        utime.into(),
                    )?;

                    let validator_subset = ValidatorSet::new(0, 0, 0, val_subset)
                        .context("Failed to create validator set")?;

                    let nodes_data = validator_subset
                        .list()
                        .iter()
                        .map(|x| {
                            let key = everscale_crypto::tl::PublicKey::Ed25519 {
                                key: x.public_key.as_slice(),
                            };
                            (
                                x.adnl_addr
                                    .unwrap_or_else(|| {
                                        let adnl = tl_proto::hash(key);
                                        UInt256::from(adnl)
                                    })
                                    .inner(),
                                *x.public_key.as_slice(),
                            )
                        })
                        .collect::<Vec<_>>();

                    let session_options_hash = compute_session_opts_hash(shard_state_extra)?;
                    let session = calculate_catchain_session_hash(
                        validator_subset.clone(),
                        shard,
                        block_info.prev_key_block_seqno(),
                        catchain_seqno,
                        session_options_hash,
                    )?;

                    let node_ids: Vec<[u8; 32]> =
                        nodes_data.iter().map(|(adnl, _)| *adnl).collect::<Vec<_>>();

                    let validator_subset_infos = get_validator_infos(
                        nodes_data,
                        elector_account.as_ref(),
                        &self.dht,
                        &self.memory_storage,
                    )
                    .await?;

                    let private_overlay_hash =
                        IdFull::for_catchain_overlay(&session, node_ids.iter());

                    let stats = PrivateOverlayStats {
                        overlay_id: hex::encode(private_overlay_hash.compute_short_id().as_slice()),
                        workchain_id: shard.workchain_id(),
                        shard_id: hex::encode(shard.shard_prefix_with_tag().to_be_bytes()),
                        catchain_seqno,
                        validator_subset_infos,
                    };
                    private_overlays.push(stats);
                }
                self.metrics.update_private_overlays(private_overlays);
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

async fn get_validator_infos(
    nodes_data: Vec<([u8; 32], [u8; 32])>,
    elector_account: Option<&ShardAccount>,
    dht: &Arc<dht::Node>,
    memory_storage: &MemoryStorage,
) -> Result<Vec<ValidatorInfo>> {
    let sem = Arc::new(tokio::sync::Semaphore::new(50));
    let mut node_addresses_ordered = FuturesOrdered::new();

    for (val, key) in nodes_data.iter() {
        let validator_address = if let Some(elector) = elector_account {
            ValidatorInfo::get_address(key, elector)?.map(hex::encode)
        } else {
            None
        };
        let node_id = NodeIdShort::from(*val);

        let sem = sem.clone();

        let future = async move {
            let _g = sem.acquire().await;
            match memory_storage.get(val) {
                Some(address) => ValidatorInfo {
                    adnl_address: hex::encode(val),
                    known_ip_address: Some(address.to_string()),
                    address: validator_address,
                    bad_validator: false,
                },
                None => match dht.find_address(&node_id).await {
                    Ok((address, _)) => {
                        memory_storage.insert_or_update_node(val, Some(address));
                        ValidatorInfo {
                            adnl_address: hex::encode(val),
                            known_ip_address: Some(address.to_string()),
                            address: validator_address,
                            bad_validator: false,
                        }
                    }

                    Err(e) => {
                        tracing::warn!(
                            "Failed to find address for node: {}. Error: {:?}",
                            hex::encode(val),
                            e
                        );

                        ValidatorInfo {
                            adnl_address: hex::encode(val),
                            known_ip_address: None,
                            address: validator_address,
                            bad_validator: false,
                        }
                    }
                },
            }
        };
        node_addresses_ordered.push_back(future);
    }

    let validator_subset_infos = node_addresses_ordered
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect();

    Ok(validator_subset_infos)
}

fn compute_session_opts_hash(mc_block_extra: &McStateExtra) -> Result<[u8; 32]> {
    let consensus_config = match mc_block_extra.config.config(29)? {
        Some(ConfigParamEnum::ConfigParam29(ccc)) => ccc.consensus_config,
        _ => return Err(anyhow!("no CatchainConfig in config_params")),
    };

    let options_new = ConfigNew {
        catchain_idle_timeout: Duration::from_millis(consensus_config.consensus_timeout_ms.into())
            .as_secs_f64(),
        catchain_max_deps: consensus_config.catchain_max_deps,
        round_candidates: consensus_config.round_candidates,
        next_candidate_delay: Duration::from_millis(
            consensus_config.next_candidate_delay_ms.into(),
        )
        .as_secs_f64(),
        round_attempt_duration: consensus_config.attempt_duration,
        max_round_attempts: consensus_config.fast_attempts,
        max_block_size: consensus_config.max_block_bytes,
        max_collated_data_size: consensus_config.max_collated_bytes,
        new_catchain_ids: consensus_config.new_catchain_ids,
    };
    let serialized_options = tl_proto::serialize(options_new);
    Ok(UInt256::calc_file_hash(serialized_options.as_slice()).inner())
}

fn calculate_catchain_session_hash(
    validators: ValidatorSet,
    shard: ShardIdent,
    last_key_block_seqno: u32,
    catchain_seqno: u32,
    session_config_hash: [u8; 32],
) -> Result<[u8; 32]> {
    let group_members = validators
        .list()
        .iter()
        .map(|validator| GroupMember {
            public_key_hash: validator
                .public_key
                .hash()
                .unwrap_or(UInt256::default())
                .inner(),
            adnl: validator.adnl_addr.unwrap_or_default().inner(),
            weight: validator.weight,
        })
        .collect::<Vec<_>>();

    let group = GroupNew {
        workchain: shard.workchain_id(),
        shard: shard.shard_prefix_with_tag(),
        vertical_seqno: 0,

        last_key_block_seqno,
        catchain_seqno,
        config_hash: session_config_hash,
        members: group_members,
    };

    let bytes = tl_proto::hash(group);

    Ok(bytes)
}

#[async_trait::async_trait]
impl Subscriber for TonSubscriber {
    async fn process_block(&self, ctx: ProcessBlockContext<'_>) -> Result<()> {
        let state = match ctx.shard_state_stuff() {
            Some(state) => state,
            None => return Ok(()),
        };

        if let Err(e) = self.update_metrics(ctx.block_stuff(), state).await {
            tracing::error!("failed to update metrics: {e:?}");
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
