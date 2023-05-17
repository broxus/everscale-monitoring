use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use arc_swap::ArcSwapOption;
use everscale_network::adnl::NodeIdShort;
use everscale_network::dht;
use everscale_network::overlay::IdFull;
use futures::StreamExt;
use rustc_hash::FxHashSet;
use tl_proto::TlWrite;
use tokio::sync::watch;
use ton_block::{
    ConfigParams, ConsensusConfig, GetRepresentationHash, HashmapAugType, ShardIdent,
    ValidatorDescr,
};
use ton_indexer::utils::*;
use ton_indexer::*;
use ton_types::UInt256;

use super::elector;
use super::metrics::*;

pub struct TonSubscriber {
    metrics: Arc<MetricsState>,
    last_config: ArcSwapOption<ConfigParams>,
    elector_address: ton_block::MsgAddressInt,
    elector_address_hash: UInt256,
    first_mc_block: AtomicBool,
    validators_to_resolve: VsetTx,
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

        let (validators_to_resolve, _) = watch::channel(Default::default());

        Arc::new(Self {
            metrics,
            last_config: Default::default(),
            elector_address,
            elector_address_hash,
            first_mc_block: AtomicBool::new(true),
            validators_to_resolve,
        })
    }

    pub async fn start(&self, engine: &Engine) -> Result<()> {
        let last_key_block = engine.load_last_key_block().await;

        match last_key_block {
            Ok(last_key_block) => {
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
                self.last_config
                    .compare_and_swap(&None::<Arc<_>>, Some(Arc::new(config.clone())));
            }
            Err(_) => tracing::warn!("last key block not found"),
        }

        let dht = engine.network().dht().clone();
        let rx = self.validators_to_resolve.subscribe();
        tokio::spawn(resolve_node_addresses(self.metrics.clone(), dht, rx));

        Ok(())
    }

    fn update_last_config(
        &self,
        block_id: &ton_block::BlockIdExt,
        config: Arc<ConfigParams>,
    ) -> Result<()> {
        self.metrics
            .update_config_metrics(block_id.seq_no, config.as_ref())
            .context("Failed to update config metrics")?;
        self.last_config.store(Some(config));
        Ok(())
    }

    async fn update_metrics(
        &self,
        engine: &Engine,
        block: &BlockStuff,
        state: &ShardStateStuff,
    ) -> Result<()> {
        // Prepare context
        let block_id = block.id();
        let block = block.block();
        let seqno = block_id.seq_no;
        let shard_tag = block_id.shard_id.shard_prefix_with_tag();
        let is_masterchain = block_id.is_masterchain();
        let extra = block.read_extra()?;
        let info = block.read_info()?;

        let block_info = block.read_info()?;
        let utime = block_info.gen_utime().as_u32();
        let software_version = block_info.gen_software().map(|v| v.version);

        // Count transactions and messages
        let mut transaction_count = 0;
        let mut message_count = 0;

        let mut update_elector_state = block_info.key_block();

        let mut in_msg_count: u32 = 0;

        extra.read_in_msg_descr()?.iterate_objects(|_| {
            in_msg_count += 1;
            Ok(true)
        })?;

        let mut out_msgs_count: u32 = 0;

        extra.read_out_msg_descr()?.iterate_objects(|x| {
            let message = x.read_message()?;
            if let Some(message) = message {
                if message.is_inbound_external() {
                    out_msgs_count += 1;
                }
            }
            Ok(true)
        })?;

        let out_in_message_ratio: f64 = if in_msg_count == 0 {
            0.0
        } else {
            out_msgs_count as f64 / in_msg_count as f64
        };

        let account_blocks = extra.read_account_blocks()?;
        let mut account_blocks_count = 0;
        account_blocks.iterate_objects(|block| {
            account_blocks_count += 1;
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
                                && value.grams.as_u128() > ELECTOR_UPDATE_THRESHOLD
                                && src.workchain_id() == ton_block::MASTERCHAIN_ID
                                && dst == &self.elector_address
                        );
                    }
                    Ok(true)
                })?;
            Ok(true)
        })?;

        let account_message_ratio: f64 = if out_msgs_count == 0 {
            0.0
        } else {
            account_blocks_count as f64 / out_msgs_count as f64
        };

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

            let mut shards: Vec<ShardIdent> = Vec::new();
            mc_extra.hashes().iterate_shards(|shard, _| {
                shards.push(shard);
                Ok(true)
            })?;

            self.metrics.update_masterchain_stats(MasterChainStats {
                shard_count: shards.len(),
                seqno,
                utime,
                transaction_count,
            });

            if let Some(persistent_state) = engine.current_persistent_state_meta() {
                self.metrics.update_persistent_state(PersistentStateInfo {
                    seqno: persistent_state.0,
                    gen_utime: persistent_state.1.gen_utime(),
                })
            }

            let elector_account = state
                .state()
                .read_accounts()
                .context("Failed to read shard accounts")?
                .get(&self.elector_address_hash)
                .context("Failed to get elector account")?;

            let is_first_mc_block = self.first_mc_block.load(Ordering::Acquire);
            if update_elector_state || is_first_mc_block {
                if let Some(account) = &elector_account {
                    self.metrics
                        .update_elections_state(account)
                        .context("Failed to update elector state")?;
                }
                self.first_mc_block.store(false, Ordering::Release);
            }

            let config = match mc_extra.config() {
                Some(config) => {
                    let config = Arc::new(config.clone());
                    self.update_last_config(block_id, config.clone())?;
                    config
                }
                None => match self.last_config.load_full() {
                    Some(config) => config,
                    None => return Ok(()),
                },
            };

            let catchain_config = config
                .catchain_config()
                .context("Failed to get catchain config")?;
            let consensus_config = config
                .consensus_config()
                .context("Failed to get consensus config")?;
            let validator_set = config
                .validator_set()
                .map(Arc::new)
                .context("Failed to get full validator set")?;

            // Update validator set only for key blocks of the first mc block
            if block_info.key_block() || is_first_mc_block {
                if let Some(account) = &elector_account {
                    let elector = elector::parse_elector_data_full(account)?;

                    let past_elections = elector.past_elections.range(..utime).last();

                    if let Some((_, past_elections)) = past_elections {
                        let mut validators = Vec::with_capacity(past_elections.frozen_dict.len());

                        for validator in validator_set.list() {
                            let info = past_elections
                                .frozen_dict
                                .get(&ton_types::UInt256::from(validator.public_key.as_slice()));

                            validators.push(ValidatorInfo {
                                public_key: *validator.public_key.as_slice(),
                                adnl: validator.adnl_addr.unwrap_or_default().inner(),
                                wallet: info.map(|x| x.addr.inner()).unwrap_or_default(),
                                stake: info.map(|x| x.stake).unwrap_or_default(),
                            });
                        }

                        self.metrics.update_validators(validators);
                        self.validators_to_resolve
                            .send_replace(validator_set.clone());
                    }
                }
            }

            let mut private_overlays: Vec<PrivateOverlayStats> = Vec::new();
            for shard in std::iter::once(ShardIdent::masterchain()).chain(shards) {
                let (subset, _) = validator_set.calc_subset(
                    &catchain_config,
                    shard.shard_prefix_with_tag(),
                    shard.workchain_id(),
                    catchain_seqno,
                    utime.into(),
                )?;

                let session = calculate_catchain_session_hash(
                    shard,
                    &subset,
                    block_info.prev_key_block_seqno(),
                    catchain_seqno,
                    &consensus_config,
                );

                let catchain_nodes = subset.iter().map(|x| match &x.adnl_addr {
                    Some(x) => x.as_slice(),
                    None => &[0; 32],
                });
                let private_overlay_hash = IdFull::for_catchain_overlay(&session, catchain_nodes);

                let validators = subset
                    .into_iter()
                    .map(|item| PrivateOverlayEntry {
                        public_key: *item.public_key.as_slice(),
                    })
                    .collect();

                let stats = PrivateOverlayStats {
                    overlay_id: private_overlay_hash.compute_short_id().into(),
                    workchain: shard.workchain_id(),
                    shard_tag: shard.shard_prefix_with_tag(),
                    catchain_seqno,
                    validators,
                };
                private_overlays.push(stats);
            }

            self.metrics.update_private_overlays(private_overlays);
        } else {
            // Update shard chains metrics
            let stats = ShardChainStats {
                shard_tag,
                seqno,
                utime,
                transaction_count,
                account_blocks_count,
                account_message_ratio,
                out_in_message_ratio,
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

async fn resolve_node_addresses(metrics: Arc<MetricsState>, dht: Arc<dht::Node>, mut rx: VsetRx) {
    use futures::future::{select, Either};
    use futures::stream::FuturesUnordered;

    const INTERVAL: Duration = Duration::from_secs(5);
    const MAX_PARALLEL_REQUESTS: usize = 50;

    let metrics = &metrics;
    let dht = &dht;

    let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_PARALLEL_REQUESTS));

    let mut remaining = FxHashSet::default();
    loop {
        let validator_set = rx.borrow_and_update().clone();

        metrics.clear_validator_ips();
        remaining.clear();
        for validator in validator_set.list() {
            remaining.insert(*validator.public_key.as_slice());
        }

        let resolve = async {
            loop {
                let mut futures = FuturesUnordered::new();
                for validator in validator_set.list() {
                    let public_key = validator.public_key.as_slice();
                    if !remaining.contains(public_key) {
                        continue;
                    }

                    let peer_id = match &validator.adnl_addr {
                        Some(adnl) => NodeIdShort::new(*adnl.as_slice()),
                        None => {
                            remaining.remove(public_key);
                            continue;
                        }
                    };

                    let semaphore = semaphore.clone();
                    futures.push(async move {
                        let _permit = semaphore.acquire().await;

                        match dht.find_address(&peer_id).await {
                            Ok((socket_addr, _)) => {
                                metrics.update_validator_ip(public_key, socket_addr);
                                Some(public_key)
                            }
                            Err(e) => {
                                tracing::debug!(
                                    %peer_id,
                                    "failed to find validator address: {e:?}",
                                );
                                None
                            }
                        }
                    });
                }

                while let Some(public_key) = futures.next().await {
                    if let Some(public_key) = public_key {
                        remaining.remove(public_key);
                    }
                }

                if remaining.is_empty() {
                    return;
                }

                tokio::time::sleep(INTERVAL).await;
            }
        };
        tokio::pin!(resolve);
        tokio::pin!(let changed = rx.changed(););

        let res = match select(resolve, changed).await {
            Either::Left((_, changed)) => {
                tracing::info!("all validator addresses are resolved, waiting for a new vset");
                changed.await
            }
            Either::Right((changed, _)) => {
                tracing::warn!("cancelled validator addresses resolver due to new vset");
                changed
            }
        };
        if res.is_err() {
            return;
        }
    }
}

fn calculate_catchain_session_hash(
    shard: ShardIdent,
    validators: &[ValidatorDescr],
    last_key_block_seqno: u32,
    catchain_seqno: u32,
    consensus_config: &ConsensusConfig,
) -> [u8; 32] {
    #[derive(TlWrite)]
    #[tl(boxed, id = "validatorSession.configNew", scheme = "scheme.tl")]
    struct ConfigNew {
        catchain_idle_timeout: f64,
        catchain_max_deps: u32,
        round_candidates: u32,
        next_candidate_delay: f64,
        round_attempt_duration: u32,
        max_round_attempts: u32,
        max_block_size: u32,
        max_collated_data_size: u32,
        new_catchain_ids: bool,
    }

    #[derive(TlWrite)]
    #[tl(boxed, id = "validator.groupMember", scheme = "scheme.tl")]
    struct GroupMember {
        public_key_hash: [u8; 32],
        adnl: [u8; 32],
        weight: u64,
    }

    #[derive(TlWrite)]
    #[tl(boxed, id = "validator.groupNew", scheme = "scheme.tl")]
    pub struct GroupNew {
        workchain: i32,
        shard: u64,
        vertical_seqno: u32,
        last_key_block_seqno: u32,
        catchain_seqno: u32,
        config_hash: [u8; 32],
        members: Vec<GroupMember>,
    }

    let config = ConfigNew {
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

    let config_hash = tl_proto::hash(config);

    let members = validators
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
        config_hash,
        members,
    };

    tl_proto::hash(group)
}

#[async_trait::async_trait]
impl Subscriber for TonSubscriber {
    async fn process_block(&self, ctx: ProcessBlockContext<'_>) -> Result<()> {
        let state = match ctx.shard_state_stuff() {
            Some(state) => state,
            None => return Ok(()),
        };

        if let Err(e) = self
            .update_metrics(ctx.engine(), ctx.block_stuff(), state)
            .await
        {
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

type VsetTx = watch::Sender<Arc<ton_block::ValidatorSet>>;
type VsetRx = watch::Receiver<Arc<ton_block::ValidatorSet>>;

const ELECTOR_UPDATE_THRESHOLD: u128 = 100_000 * ONE_EVER;
const ONE_EVER: u128 = 1_000_000_000;
