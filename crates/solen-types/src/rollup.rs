//! Rollup domain types.

use serde::{Deserialize, Serialize};

use crate::{AccountId, Hash, RollupId};

/// Proof mechanism used by a rollup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProofType {
    Validity,
    Fraud,
}

/// Registered rollup domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollupRegistration {
    pub id: RollupId,
    pub name: String,
    pub proof_type: ProofType,
    pub vm_type: String,
    pub sequencer: AccountId,
    pub bridge_config: BridgeConfig,
}

/// Bridge configuration for a rollup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeConfig {
    pub vault_account: AccountId,
    pub challenge_window_blocks: u64,
    pub withdrawal_delay_blocks: u64,
}

/// A batch commitment published by a rollup sequencer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchCommitment {
    pub rollup_id: RollupId,
    pub batch_index: u64,
    pub state_root: Hash,
    pub data_hash: Hash,
    pub proof: Vec<u8>,
}
