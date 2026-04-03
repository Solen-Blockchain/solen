//! Gas / resource metering via wasmtime's fuel mechanism.

/// Default fuel limit for contract execution (roughly maps to gas).
pub const DEFAULT_FUEL_LIMIT: u64 = 1_000_000;

/// Fuel cost per storage write: base cost + per-byte cost.
/// Discourages unbounded state growth.
pub const STORAGE_WRITE_BASE_FUEL: u64 = 2_000;
pub const STORAGE_WRITE_PER_BYTE_FUEL: u64 = 10;

/// Fuel cost per storage read: cheaper than writes.
pub const STORAGE_READ_BASE_FUEL: u64 = 500;

/// Convert wasmtime fuel consumed to Solen gas units.
/// Currently 1:1 mapping; can be adjusted for different pricing.
pub fn fuel_to_gas(fuel_consumed: u64) -> u64 {
    fuel_consumed
}

/// Calculate fuel cost for a storage write.
pub fn storage_write_fuel(key_len: usize, val_len: usize) -> u64 {
    STORAGE_WRITE_BASE_FUEL + ((key_len + val_len) as u64) * STORAGE_WRITE_PER_BYTE_FUEL
}

/// Calculate fuel cost for a storage read.
pub fn storage_read_fuel(key_len: usize) -> u64 {
    STORAGE_READ_BASE_FUEL + (key_len as u64) * STORAGE_WRITE_PER_BYTE_FUEL
}
