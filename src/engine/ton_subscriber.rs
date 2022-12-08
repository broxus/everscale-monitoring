use std::collections::HashMap;
use std::net::SocketAddrV4;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use arc_swap::ArcSwapOption;
use everscale_network::adnl::NodeIdShort;
use everscale_network::dht;
use everscale_network::overlay::IdFull;
use futures::stream::{FuturesOrdered, StreamExt};
use nekoton_abi::{KnownParamType, UnpackAbi};
use once_cell::race::OnceBox;
use tl_proto::TlWrite;
use ton_abi::TokenValue;
use ton_block::{
    ConfigParams, ConsensusConfig, GetRepresentationHash, HashmapAugType, ShardAccount, ShardIdent,
    ValidatorDescr,
};
use ton_indexer::utils::*;
use ton_indexer::*;
use ton_types::{FxDashMap, UInt256};

use super::metrics::*;

#[derive(Clone, Default)]
pub struct CurrentValidatorDescription {
    pub node_adnl_address: [u8; 32],
    pub node_pub_key: [u8; 32],
    pub contract_address: [u8; 32],
    pub node_ip_address: Option<SocketAddrV4>,
}

pub struct TonSubscriber {
    metrics: Arc<MetricsState>,
    last_config: ArcSwapOption<ConfigParams>,
    elector_address: ton_block::MsgAddressInt,
    elector_address_hash: ton_types::UInt256,
    first_mc_block: AtomicBool,
    dht: ArcSwapOption<dht::Node>,
    validators: Arc<ValidatorSetStorage>,
}

impl TonSubscriber {
    pub fn new(metrics: Arc<MetricsState>, dht: ArcSwapOption<dht::Node>) -> Arc<Self> {
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
            last_config: Default::default(),
            elector_address,
            elector_address_hash,
            first_mc_block: AtomicBool::new(true),
            dht,
            validators: Arc::new(ValidatorSetStorage::default()),
        })
    }

    pub async fn start(&self, engine: &Engine) -> Result<()> {
        self.dht.store(Some(engine.network().dht().clone()));

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
        self.update_last_config(last_key_block.id(), Arc::new(config.clone()))?;

        let storage = self.validators.clone();

        if let Some(dht) = self.dht.load().clone() {
            begin_polling_node_addresses(storage, dht);
        }

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

            let elector_account = state
                .state()
                .read_accounts()
                .context("Failed to read shard accounts")?
                .get(&self.elector_address_hash)
                .context("Failed to get elector account")?;

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
                .context("Failed to get full validator set")?;

            if update_elector_state || self.first_mc_block.load(Ordering::Acquire) {
                if let Some(account) = elector_account.as_ref() {
                    self.metrics
                        .update_elections_state(account)
                        .context("Failed to update elector state")?;

                    self.first_mc_block.store(false, Ordering::Release);
                    self.validators.clear();
                    let elector_state = parse_elector_state(account)?;

                    let validators_key_map = validator_set
                        .list()
                        .iter()
                        .map(|x| (*x.public_key.as_slice(), x.adnl_addr.unwrap_or_default()))
                        .collect::<HashMap<[u8; 32], UInt256>>();

                    let past_elections = elector_state
                        .past_elections
                        .iter()
                        .max_by(|(a, _), (b, _)| a.cmp(b))
                        .filter(|(ts, _)| (**ts as u64) < broxus_util::now_sec_u64());

                    if let Some((_, past_elections)) = past_elections {
                        for (key, stake) in past_elections.frozen_dict.iter() {
                            let adnl_addr_opt = validators_key_map.get(&key.inner());
                            let descr = CurrentValidatorDescription {
                                node_adnl_address: adnl_addr_opt
                                    .map(|x| x.inner())
                                    .unwrap_or_default(),
                                node_pub_key: key.inner(),
                                contract_address: stake.addr.inner(),
                                node_ip_address: None,
                            };
                            self.validators.set(key.as_slice(), descr);
                        }
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

                let catchain_nodes = subset
                    .iter()
                    .map(|x| x.adnl_addr.unwrap_or_default().inner())
                    .collect::<Vec<_>>();

                let private_overlay_hash =
                    IdFull::for_catchain_overlay(&session, catchain_nodes.iter());

                let mut validator_subset_infos = Vec::new();
                for s in subset {
                    let description = self.validators.get(s.public_key.as_slice());
                    if let Some(description) = description {
                        let key = everscale_crypto::tl::PublicKey::Ed25519 {
                            key: &description.node_pub_key,
                        };

                        let adnl = tl_proto::hash(key);
                        let validator_info = ValidatorInfo {
                            adnl_address: hex::encode(adnl),
                            known_ip_address: description.node_ip_address.map(|x| x.to_string()),
                            address: Some(hex::encode(description.contract_address)),
                        };
                        validator_subset_infos.push(validator_info);
                    }
                }

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

fn begin_polling_node_addresses(validators_storage: Arc<ValidatorSetStorage>, dht: Arc<dht::Node>) {
    tokio::spawn(async move {
        loop {
            let validators_storage = validators_storage.clone();
            let validators = validators_storage.get_validators_with_empty_ips();

            let sem = Arc::new(tokio::sync::Semaphore::new(50));
            let mut node_addresses_ordered = FuturesOrdered::new();

            for val in validators.iter() {
                let sem = sem.clone();
                let node_short_id = NodeIdShort::new(val.node_adnl_address);

                let dht_clone = &dht;
                let storage = validators_storage.as_ref();
                let future = async move {
                    let _g = sem.acquire().await;
                    match &dht_clone.find_address(&node_short_id).await {
                        Ok((address, _)) => {
                            let new = CurrentValidatorDescription {
                                node_adnl_address: val.node_adnl_address,
                                node_pub_key: val.node_pub_key,
                                contract_address: val.contract_address,
                                node_ip_address: Some(*address),
                            };
                            storage.set(&val.node_pub_key, new);
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to find address for node: {}. Error: {:?}",
                                hex::encode(val.node_adnl_address),
                                e
                            );
                        }
                    };
                };
                node_addresses_ordered.push_back(future);
            }

            node_addresses_ordered.collect::<Vec<_>>().await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

fn parse_elector_state(elector_account: &ShardAccount) -> Result<ElectorData> {
    let account = match elector_account.read_account()? {
        ton_block::Account::Account(account) => account,
        ton_block::Account::AccountNone => anyhow::bail!("Failed to read elector account"),
    };

    let state = match account.storage.state {
        ton_block::AccountState::AccountActive { state_init, .. } => state_init,
        _ => anyhow::bail!("Elector account is not active"),
    };

    let data = match state.data {
        Some(data) => data,
        None => anyhow::bail!("Elector account is not active"),
    };

    fn parse_elector_data(data: ton_types::Cell) -> Result<ElectorData> {
        static PARAM_TYPE: OnceBox<ton_abi::ParamType> = OnceBox::new();
        let param_type = PARAM_TYPE.get_or_init(|| Box::new(ElectorData::param_type()));

        let (elector_data, _) = TokenValue::read_from(
            param_type,
            data.into(),
            true,
            &ton_abi::contract::ABI_VERSION_2_1,
            false,
        )
        .context("Failed to read elector data")?;

        elector_data
            .unpack()
            .context("Failed to unpack elector data")
    }

    parse_elector_data(data).context("Failed to parse decoded past elections data")
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
    pub struct ConfigNew {
        pub catchain_idle_timeout: f64,
        pub catchain_max_deps: u32,
        pub round_candidates: u32,
        pub next_candidate_delay: f64,
        pub round_attempt_duration: u32,
        pub max_round_attempts: u32,
        pub max_block_size: u32,
        pub max_collated_data_size: u32,
        pub new_catchain_ids: bool,
    }

    #[derive(TlWrite)]
    #[tl(boxed, id = "validator.groupMember", scheme = "scheme.tl")]
    pub struct GroupMember {
        pub public_key_hash: [u8; 32],
        pub adnl: [u8; 32],
        pub weight: u64,
    }

    #[derive(TlWrite)]
    #[tl(boxed, id = "validator.groupNew", scheme = "scheme.tl")]
    pub struct GroupNew {
        pub workchain: i32,
        pub shard: u64,
        pub vertical_seqno: u32,
        pub last_key_block_seqno: u32,
        pub catchain_seqno: u32,
        pub config_hash: [u8; 32],
        pub members: Vec<GroupMember>,
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

        if let Err(e) = self.update_metrics(ctx.block_stuff(), state).await {
            tracing::error!("failed to update metrics: {e:?}");
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct ValidatorSetStorage {
    validator_set: FxDashMap<[u8; 32], CurrentValidatorDescription>,
}

impl ValidatorSetStorage {
    pub fn get(&self, public_key: &[u8; 32]) -> Option<CurrentValidatorDescription> {
        self.validator_set.get(public_key).map(|x| x.clone())
    }

    pub fn get_validators_with_empty_ips(&self) -> Vec<CurrentValidatorDescription> {
        self.validator_set
            .iter()
            .filter(|x| x.node_ip_address.is_none())
            .map(|x| x.clone())
            .collect::<Vec<_>>()
    }

    pub fn set(&self, public_key: &[u8; 32], node_info: CurrentValidatorDescription) {
        self.validator_set.insert(*public_key, node_info);
    }

    pub fn clear(&self) {
        self.validator_set.clear();
    }
}

#[derive(thiserror::Error, Debug)]
enum TonSubscriberError {
    #[error("Given block is not a master block")]
    NotAMasterChainBlock,
}

const ELECTOR_UPDATE_THRESHOLD: u128 = 100_000 * ONE_EVER;
const ONE_EVER: u128 = 1_000_000_000;

//type ShardDescrTreeRef = ton_block::InRefValue<ton_block::BinTree<ton_block::ShardDescr>>;
