//! Host functions exposed to guest WASM contracts.
//!
//! These are the syscalls that contracts use to interact with chain state.

use std::collections::HashMap;

use solen_types::AccountId;

/// Mutable state context passed to host functions during execution.
pub struct HostContext {
    pub caller: AccountId,
    pub contract_id: AccountId,
    pub block_height: u64,
    /// Contract-local storage (key -> value).
    pub storage: HashMap<Vec<u8>, Vec<u8>>,
    /// Events emitted during execution.
    pub events: Vec<HostEvent>,
    /// Return data from the contract.
    pub return_data: Vec<u8>,
    /// Native SOLEN transfers initiated by the contract.
    /// Processed by the executor after WASM execution completes.
    pub native_transfers: Vec<NativeTransfer>,
}

/// A native SOLEN transfer initiated by a contract via host function.
#[derive(Debug, Clone)]
pub struct NativeTransfer {
    pub to: AccountId,
    pub amount: u128,
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
            contract_id: [0u8; 32],
            block_height,
            storage: HashMap::new(),
            events: Vec::new(),
            return_data: Vec::new(),
            native_transfers: Vec::new(),
        }
    }

    pub fn with_contract_id(mut self, id: AccountId) -> Self {
        self.contract_id = id;
        self
    }

    /// Pre-populate storage from existing contract state.
    pub fn with_storage(mut self, storage: HashMap<Vec<u8>, Vec<u8>>) -> Self {
        self.storage = storage;
        self
    }
}
