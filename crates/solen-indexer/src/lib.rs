//! Event indexer: processes finalized blocks, indexes events and receipts,
//! and serves them via a REST API for the block explorer.

pub mod api;
pub mod indexer;
pub mod store;
pub mod stsolen_apy;
