//! Host functions exposed to guest WASM contracts.
//!
//! These are the syscalls that contracts use to interact with chain state.

use std::collections::HashMap;

use solen_types::AccountId;

/// Mutable state context passed to host functions during execution.
pub struct HostContext {
    pub caller: AccountId,
    pub block_height: u64,
    /// Contract-local storage (key -> value).
    pub storage: HashMap<Vec<u8>, Vec<u8>>,
    /// Events emitted during execution.
    pub events: Vec<HostEvent>,
    /// Return data from the contract.
    pub return_data: Vec<u8>,
}

/// An event emitted by a contract via host functions.
#[derive(Debug, Clone)]
pub struct HostEvent {
    pub topic: Vec<u8>,
    pub data: Vec<u8>,
}

impl HostContext {
    pub fn new(caller: AccountId, block_height: u64) -> Self {
        Self {
            caller,
            block_height,
            storage: HashMap::new(),
            events: Vec::new(),
            return_data: Vec::new(),
        }
    }

    /// Pre-populate storage from existing contract state.
    pub fn with_storage(mut self, storage: HashMap<Vec<u8>, Vec<u8>>) -> Self {
        self.storage = storage;
        self
    }
}
