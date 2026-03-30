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

/// Vesting system contract — holds team/investor tokens with time locks.
pub const VESTING_ADDRESS: AccountId = addr(0x06);

/// Staking rewards pool — holds the 500M SOLEN allocated for validator rewards.
pub const STAKING_POOL_ADDRESS: AccountId = addr(0x10);

// ── Fund account addresses (non-contract, just regular accounts) ──

/// Ecosystem fund account.
pub const ECOSYSTEM_FUND_ADDRESS: AccountId = addr(0x20);

/// Community & airdrops account.
pub const COMMUNITY_ADDRESS: AccountId = addr(0x21);

/// Liquidity & market making account.
pub const LIQUIDITY_ADDRESS: AccountId = addr(0x22);

/// Team & founders vesting pool (tokens held by vesting contract).
pub const TEAM_POOL_ADDRESS: AccountId = addr(0x23);

/// Early investors vesting pool (tokens held by vesting contract).
pub const INVESTOR_POOL_ADDRESS: AccountId = addr(0x24);

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
pub const ALL_SYSTEM_ADDRESSES: [AccountId; 6] = [
    STAKING_ADDRESS,
    GOVERNANCE_ADDRESS,
    BRIDGE_ADDRESS,
    TREASURY_ADDRESS,
    INTENT_ADDRESS,
    VESTING_ADDRESS,
];
