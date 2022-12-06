// Generic stuff
////////////////////////////////////////////////////////////////////////////////

---types---

int ? = Int;
long ? = Long;
double ? = Double;
string ? = String;
object ? = Object;
bytes data:string = Bytes;
boolTrue = Bool;
boolFalse = Bool;

vector {t:Type} # [ t ] = Vector t;

int256 8*[ int ] = Int256;


// PRIVATE OVERLAY
////////////////////////////////////////////////////////////////////////////////

---types---

validator.groupMember
    public_key_hash:int256
    adnl:int256
    weight:long
        = engine.validator.GroupMember;

validator.groupNew
    workchain:int
    shard:long
    vertical_seqno:int
    last_key_block_seqno:int
    catchain_seqno:int
    config_hash:int256
    members:(vector validator.groupMember)
        = validator.Group;

validatorSession.configNew
    catchain_idle_timeout:double
    catchain_max_deps:int
    round_candidates:int
    next_candidate_delay:double
    round_attempt_duration:int
    max_round_attempts:int
    max_block_size:int
    max_collated_data_size:int
    new_catchain_ids:Bool
        = validatorSession.Config;
