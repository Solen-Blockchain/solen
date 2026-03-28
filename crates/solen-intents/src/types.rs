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
}
