//! Core types shared across all Solen crates.

pub mod account;
pub mod block;
pub mod crypto;
pub mod encoding;
pub mod rollup;
pub mod system;
pub mod transaction;

/// Fixed-size 32-byte hash.
pub type Hash = [u8; 32];

/// Account identifier (32 bytes, derived from code hash + salt).
pub type AccountId = [u8; 32];

/// Validator identifier.
pub type ValidatorId = [u8; 32];

/// Rollup domain identifier.
pub type RollupId = u64;

/// Block height.
pub type BlockHeight = u64;

/// Epoch number.
pub type Epoch = u64;
