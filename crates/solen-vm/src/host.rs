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
    /// Contract→contract calls queued via `sdk::queue_call`. Drained and
    /// dispatched by the executor AFTER this contract's `call()` returns —
    /// so they cannot re-enter the queueing contract. Each pending call runs
    /// with `caller = this contract_id`, and can itself queue further calls
    /// (subject to the executor's depth cap).
    pub pending_calls: Vec<PendingCall>,
    /// Sum of preceding unconsumed `Action::Transfer { to: self }` amounts in
    /// the current UserOperation. Each `Action::Call` consumes and resets this
    /// counter at dispatch time, so it reflects exactly the native SOLEN moved
    /// into this contract since the last Call to it. Equivalent to EVM's
    /// `msg.value`; stays constant throughout a single Call frame.
    pub msg_value: u128,
}

/// A native SOLEN transfer initiated by a contract via host function.
#[derive(Debug, Clone)]
pub struct NativeTransfer {
    pub to: AccountId,
    pub amount: u128,
}

/// A contract→contract call queued by a contract via host function.
/// Dispatched by the executor after the queueing contract's `call()` returns.
#[derive(Debug, Clone)]
pub struct PendingCall {
    pub target: AccountId,
    pub method: Vec<u8>,
    pub args: Vec<u8>,
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
            pending_calls: Vec::new(),
            msg_value: 0,
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

    /// Set the native SOLEN amount transferred to this contract in the current UserOp.
    pub fn with_msg_value(mut self, value: u128) -> Self {
        self.msg_value = value;
        self
    }
}
