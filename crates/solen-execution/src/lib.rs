//! Settlement execution engine.
//!
//! Processes transactions, manages state transitions, and verifies proofs
//! submitted by rollup domains.

pub mod executor;
pub mod fees;
pub mod genesis;
pub mod proof;
pub mod receipt;
pub mod state;
