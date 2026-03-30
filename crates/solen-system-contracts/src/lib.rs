//! System contracts for the Solen settlement layer.
//!
//! Privileged contracts that manage protocol-level state:
//! governance, canonical bridge, proof verifier registry, stake management,
//! and treasury.

pub mod bridge;
pub mod governance;
pub mod proof_registry;
pub mod staking;
pub mod treasury;
pub mod vesting;
