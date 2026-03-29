//! Well-known system contract addresses.
//!
//! These are constant addresses that the executor intercepts and routes
//! to native Rust implementations instead of the WASM VM.

use crate::AccountId;

/// Staking system contract.
pub const STAKING_ADDRESS: AccountId = addr(0x01);

/// Governance system contract.
pub const GOVERNANCE_ADDRESS: AccountId = addr(0x02);

/// Bridge system contract.
pub const BRIDGE_ADDRESS: AccountId = addr(0x03);

/// Treasury system contract.
pub const TREASURY_ADDRESS: AccountId = addr(0x04);

/// Intent pool system contract.
pub const INTENT_ADDRESS: AccountId = addr(0x05);

/// Staking rewards pool — holds the 500M SOLEN allocated for validator rewards.
/// Rewards are deducted from this account each epoch. When it's empty, rewards stop.
pub const STAKING_POOL_ADDRESS: AccountId = addr(0x10);

/// Generate a system address: 0xFF repeated prefix + identifier byte.
const fn addr(id: u8) -> AccountId {
    let mut a = [0xFFu8; 32];
    a[31] = id;
    a
}

/// Check if an address is a system contract.
pub fn is_system_contract(id: &AccountId) -> bool {
    // System contracts have 0xFF prefix in first 30 bytes.
    id[..30] == [0xFF; 30]
}

/// All system contract addresses.
pub const ALL_SYSTEM_ADDRESSES: [AccountId; 5] = [
    STAKING_ADDRESS,
    GOVERNANCE_ADDRESS,
    BRIDGE_ADDRESS,
    TREASURY_ADDRESS,
    INTENT_ADDRESS,
];
