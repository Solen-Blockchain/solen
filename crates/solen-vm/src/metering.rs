//! Gas / resource metering via wasmtime's fuel mechanism.

/// Default fuel limit for contract execution (roughly maps to gas).
pub const DEFAULT_FUEL_LIMIT: u64 = 1_000_000;

/// Convert wasmtime fuel consumed to Solen gas units.
/// Currently 1:1 mapping; can be adjusted for different pricing.
pub fn fuel_to_gas(fuel_consumed: u64) -> u64 {
    fuel_consumed
}
