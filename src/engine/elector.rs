use std::collections::BTreeMap;

use anyhow::{Context, Result};
use nekoton_abi::{KnownParamType, MaybeRef, UnpackAbi};
use once_cell::race::OnceBox;
use ton_types::UInt256;

pub fn parse_current_election(
    elector_account: &ton_block::ShardAccount,
) -> Result<Option<CurrentElectionData>> {
    static ABI: OnceBox<ton_abi::ParamType> = OnceBox::new();
    let params = ABI.get_or_init(|| Box::new(MaybeRef::<CurrentElectionData>::param_type()));

    let data = read_elector_data(elector_account)?;

    let (data, _) = ton_abi::TokenValue::read_from(
        params,
        data.into(),
        false,
        &ton_abi::contract::ABI_VERSION_2_1,
        true,
    )
    .context("Failed to decode elector state data")?;

    let MaybeRef(current_election) = data
        .unpack()
        .context("Failed to parse decoded elector state data")?;

    Ok(current_election)
}

pub fn parse_elector_data_full(elector_account: &ton_block::ShardAccount) -> Result<ElectorData> {
    static ABI: OnceBox<ton_abi::ParamType> = OnceBox::new();
    let params = ABI.get_or_init(|| Box::new(ElectorData::param_type()));

    let data = read_elector_data(elector_account)?;

    let (data, _) = ton_abi::TokenValue::read_from(
        params,
        data.into(),
        true,
        &ton_abi::contract::ABI_VERSION_2_1,
        false,
    )
    .context("Failed to decode elector state data")?;

    data.unpack()
        .context("Failed to parse decoded elector state data")
}

fn read_elector_data(account: &ton_block::ShardAccount) -> Result<ton_types::Cell> {
    let account = match account.read_account()? {
        ton_block::Account::Account(account) => account,
        ton_block::Account::AccountNone => anyhow::bail!("Elector account is empty"),
    };

    let state = match account.storage.state {
        ton_block::AccountState::AccountActive { state_init, .. } => state_init,
        _ => anyhow::bail!("Elector not deployed"),
    };

    match state.data {
        Some(data) => Ok(data),
        None => anyhow::bail!("Elector data is empty"),
    }
}

#[derive(Debug, UnpackAbi, KnownParamType)]
pub struct ElectorData {
    #[abi]
    pub current_election: MaybeRef<CurrentElectionData>,
    #[abi]
    pub credits: BTreeMap<UInt256, ton_block::Grams>,
    #[abi]
    pub past_elections: BTreeMap<u32, PastElectionData>,
    #[abi(gram)]
    pub grams: u128,
    #[abi(uint32)]
    pub active_id: u32,

    #[abi(uint256)]
    pub active_hash: UInt256,
}

#[derive(Debug, UnpackAbi, KnownParamType)]
pub struct PastElectionData {
    #[abi(uint32)]
    pub unfreeze_at: u32,
    #[abi(uint32)]
    pub stake_held: u32,
    #[abi(uint256)]
    pub vset_hash: UInt256,
    #[abi]
    pub frozen_dict: BTreeMap<UInt256, FrozenStake>,
    #[abi(gram)]
    pub total_stake: u128,
    #[abi(gram)]
    pub bonuses: u128,
    #[abi]
    pub complaints: BTreeMap<UInt256, Complaint>,
}

#[derive(Debug, UnpackAbi, KnownParamType)]
pub struct FrozenStake {
    #[abi(uint256)]
    pub addr: UInt256,
    #[abi(uint64)]
    pub weight: u64,
    #[abi(gram)]
    pub stake: u64,
}

#[derive(Debug, UnpackAbi, KnownParamType)]
pub struct Complaint {
    #[abi(uint8)]
    pub _header: u8,
    #[abi(cell)]
    pub complaint: ton_types::Cell,
    #[abi]
    pub voters: BTreeMap<u16, bool>,
    #[abi(uint256)]
    pub vset_id: UInt256,
    #[abi]
    pub weight_remaining: i64,
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
    #[abi]
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
    #[abi(uint256)]
    pub src_addr: UInt256,
    #[abi(uint256)]
    pub adnl_addr: UInt256,
}
