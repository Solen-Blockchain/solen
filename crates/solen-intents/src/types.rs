//! Intent types and constraint definitions.

use serde::{Deserialize, Serialize};
use solen_types::AccountId;

/// A constraint that must be satisfied for an intent to be fulfilled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Constraint {
    /// Minimum balance of an asset after execution.
    MinBalance {
        account: AccountId,
        min_amount: u128,
    },
    /// Maximum amount spent from an account.
    MaxSpend {
        account: AccountId,
        max_amount: u128,
    },
    /// A specific transfer must occur.
    RequireTransfer {
        from: AccountId,
        to: AccountId,
        min_amount: u128,
    },
    /// A specific contract method must be called.
    RequireCall {
        target: AccountId,
        method: String,
    },
    /// Cross-chain swap: lock SOLEN in bridge, solver delivers output on destination chain.
    /// The L1 verifies the lock; the solver fronts output to the user on the destination chain.
    CrossChainSwap {
        /// Amount of SOLEN the user is willing to spend (locked in bridge).
        input_amount: u128,
        /// Minimum output the solver must deliver on the destination chain.
        /// Denominated in destination token base units.
        min_output: u128,
        /// Destination chain ID (8453 = Base).
        destination_chain: u64,
        /// Recipient address on the destination chain (20 bytes for EVM, zero-padded to 32).
        destination_address: [u8; 32],
        /// Output token on destination (contract address, zero-padded to 32).
        /// Zero = native ETH, otherwise ERC-20 address.
        output_token: [u8; 32],
    },
    /// Custom constraint evaluated by a verifier contract.
    Custom {
        verifier: AccountId,
        data: Vec<u8>,
    },
}

/// A user's intent expressing a desired outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intent {
    pub id: u64,
    pub sender: AccountId,
    pub constraints: Vec<Constraint>,
    pub max_fee: u128,
    pub expiry_height: u64,
    pub signature: Vec<u8>,
    pub tip: u128, // incentive for the solver
}

/// Status of an intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntentStatus {
    Pending,
    Matched,
    Fulfilled,
    Expired,
    Cancelled,
}

/// A solver's proposed solution to an intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Solution {
    pub intent_id: u64,
    pub solver: AccountId,
    /// The operations the solver will execute to fulfill the intent.
    pub operations: Vec<solen_types::transaction::UserOperation>,
    /// How much of the tip the solver claims.
    pub claimed_tip: u128,
    pub score: u64, // solver-reported quality score
    /// Signature proving the solver controls the claimed solver account.
    /// Signs: intent_id[8] + solver[32] + claimed_tip[16].
    #[serde(default)]
    pub signature: Vec<u8>,
}
