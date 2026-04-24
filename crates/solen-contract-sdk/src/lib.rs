//! Solen Contract SDK
//!
//! Write smart contracts in Rust that compile to WASM and run on Solen.
//!
//! # Example
//!
//! ```rust,ignore
//! use solen_contract_sdk::*;
//!
//! #[no_mangle]
//! pub extern "C" fn call(input_ptr: i32, input_len: i32) -> i32 {
//!     let input = sdk::read_input(input_ptr, input_len);
//!     let count = storage::get_u64(b"count").unwrap_or(0);
//!     storage::set_u64(b"count", count + 1);
//!     events::emit(b"incremented", &count.to_le_bytes());
//!     sdk::return_value(&(count + 1).to_le_bytes())
//! }
//! ```

#![no_std]

extern "C" {
    fn storage_read(key_ptr: i32, key_len: i32, val_ptr: i32) -> i32;
    fn storage_write(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32);
    fn emit_event(topic_ptr: i32, topic_len: i32, data_ptr: i32, data_len: i32);
    fn get_caller(out_ptr: i32);
    fn get_block_height() -> i64;
    fn set_return_data(ptr: i32, len: i32);
    fn transfer_native(to_ptr: i32, amount_ptr: i32) -> i32;
    fn get_self_id(out_ptr: i32);
    fn msg_value(out_ptr: i32);
}

/// Low-level SDK functions for input/output.
pub mod sdk {
    use super::*;

    /// Read input data from the host. Returns a fixed-size buffer and the actual length.
    /// The caller is responsible for interpreting the bytes.
    pub fn read_input(ptr: i32, len: i32) -> &'static [u8] {
        unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) }
    }

    /// Set return data and return its length (for use as the `call` return value).
    pub fn return_value(data: &[u8]) -> i32 {
        unsafe {
            set_return_data(data.as_ptr() as i32, data.len() as i32);
        }
        data.len() as i32
    }

    /// Get the 32-byte account ID of the caller.
    pub fn caller() -> [u8; 32] {
        let mut buf = [0u8; 32];
        unsafe {
            get_caller(buf.as_mut_ptr() as i32);
        }
        buf
    }

    /// Get the current block height.
    pub fn block_height() -> u64 {
        unsafe { get_block_height() as u64 }
    }

    /// Transfer native SOLEN from this contract to another account.
    /// Returns true on success, false on failure.
    /// The transfer is queued and executed after WASM completes.
    pub fn transfer(to: &[u8; 32], amount: u128) -> bool {
        unsafe { transfer_native(to.as_ptr() as i32, amount.to_le_bytes().as_ptr() as i32) == 0 }
    }

    /// Get this contract's own account ID.
    pub fn self_id() -> [u8; 32] {
        let mut buf = [0u8; 32];
        unsafe {
            get_self_id(buf.as_mut_ptr() as i32);
        }
        buf
    }

    /// Native SOLEN transferred to this contract in the current UserOperation.
    ///
    /// Returns the sum of all `Action::Transfer { to: self }` in the current
    /// op since the previous `Action::Call` to this contract (or op start).
    /// Use this in deposit paths to verify a claimed amount: e.g.
    /// `assert!(claimed <= sdk::msg_value())`.
    pub fn msg_value() -> u128 {
        let mut buf = [0u8; 16];
        unsafe {
            msg_value(buf.as_mut_ptr() as i32);
        }
        u128::from_le_bytes(buf)
    }
}

/// Contract storage helpers.
pub mod storage {
    use super::*;

    // Internal scratch buffer for storage operations.
    // Contracts run single-threaded in WASM so this is safe.
    static mut SCRATCH: [u8; 4096] = [0u8; 4096];

    /// Read raw bytes from storage. Returns `None` if the key doesn't exist.
    pub fn get(key: &[u8]) -> Option<&'static [u8]> {
        unsafe {
            let len = storage_read(
                key.as_ptr() as i32,
                key.len() as i32,
                SCRATCH.as_mut_ptr() as i32,
            );
            if len < 0 {
                None
            } else {
                Some(&SCRATCH[..len as usize])
            }
        }
    }

    /// Write raw bytes to storage.
    pub fn set(key: &[u8], value: &[u8]) {
        unsafe {
            storage_write(
                key.as_ptr() as i32,
                key.len() as i32,
                value.as_ptr() as i32,
                value.len() as i32,
            );
        }
    }

    /// Read a u64 from storage.
    pub fn get_u64(key: &[u8]) -> Option<u64> {
        let bytes = get(key)?;
        if bytes.len() < 8 {
            return None;
        }
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&bytes[..8]);
        Some(u64::from_le_bytes(buf))
    }

    /// Write a u64 to storage.
    pub fn set_u64(key: &[u8], value: u64) {
        set(key, &value.to_le_bytes());
    }

    /// Read a u128 from storage.
    pub fn get_u128(key: &[u8]) -> Option<u128> {
        let bytes = get(key)?;
        if bytes.len() < 16 {
            return None;
        }
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&bytes[..16]);
        Some(u128::from_le_bytes(buf))
    }

    /// Write a u128 to storage.
    pub fn set_u128(key: &[u8], value: u128) {
        set(key, &value.to_le_bytes());
    }
}

/// Event emission helpers.
pub mod events {
    use super::*;

    /// Emit an event with a topic and data payload.
    pub fn emit(topic: &[u8], data: &[u8]) {
        unsafe {
            emit_event(
                topic.as_ptr() as i32,
                topic.len() as i32,
                data.as_ptr() as i32,
                data.len() as i32,
            );
        }
    }
}

/// Panic handler for WASM contracts (required for no_std).
#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

/// Global allocator stub — contracts use stack allocation by default.
/// For heap allocation, add a WASM allocator crate.
#[cfg(target_arch = "wasm32")]
#[global_allocator]
static ALLOC: BumpAllocator = BumpAllocator;

#[cfg(target_arch = "wasm32")]
struct BumpAllocator;

#[cfg(target_arch = "wasm32")]
unsafe impl core::alloc::GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, _layout: core::alloc::Layout) -> *mut u8 {
        core::ptr::null_mut() // Contracts should avoid heap allocation
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {}
}
