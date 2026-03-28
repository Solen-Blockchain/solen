//! WASM virtual machine with deterministic execution and resource metering.
//!
//! Uses wasmtime for WASM execution with fuel-based gas metering.
//! Contracts interact with chain state through host functions.

pub mod host;
pub mod metering;
pub mod runtime;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VmError {
    #[error("out of gas")]
    OutOfGas,
    #[error("invalid bytecode: {0}")]
    InvalidBytecode(String),
    #[error("execution trapped: {0}")]
    Trap(String),
    #[error("host error: {0}")]
    HostError(String),
    #[error("missing export: {0}")]
    MissingExport(String),
}
