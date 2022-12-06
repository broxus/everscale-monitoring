use tl_proto::{TlRead, TlWrite};

#[derive(Debug, Clone, TlRead, TlWrite)]
#[tl(boxed, id = "validator.groupMember", scheme = "scheme.tl")]
pub struct GroupMember {
    pub public_key_hash: [u8; 32],
    pub adnl: [u8; 32],
    pub weight: u64,
}

#[derive(Debug, Clone, TlRead, TlWrite)]
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

#[derive(Debug, Clone, TlRead, TlWrite)]
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
