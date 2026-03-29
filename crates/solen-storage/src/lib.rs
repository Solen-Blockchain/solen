//! State storage abstraction and implementations.

pub mod memory;
pub mod merkle;
#[cfg(feature = "rocksdb")]
pub mod rocks;
pub mod traits;

pub use memory::MemoryStore;
#[cfg(feature = "rocksdb")]
pub use rocks::RocksStore;
pub use traits::{StateStore, StorageError};
