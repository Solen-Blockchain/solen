//! BFT PoS consensus engine.
//!
//! Implements validator management, block production, finality gadget,
//! epoch transitions, slashing, and checkpointing.
//!
//! In Phase 1, this is a single-validator engine that produces blocks
//! on a fixed interval.

pub mod checkpoint;
pub mod encrypted_mempool;
pub mod engine;
pub mod epoch;
pub mod mempool;
pub mod slashing;
pub mod snapshot;
pub mod validator;
