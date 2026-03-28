//! Intent-aware execution system.
//!
//! Users submit intents that express desired outcomes (e.g., "swap X for at
//! least Y") rather than specific execution steps. Solvers compete to fulfill
//! intents under auditable rules.

pub mod pool;
pub mod solver;
pub mod types;
