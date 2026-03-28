//! Storage traits.

use solen_types::Hash;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("key not found")]
    NotFound,
    #[error("storage backend error: {0}")]
    Backend(String),
}

/// Trait for key-value state storage with Merkle commitments.
pub trait StateStore: Send + Sync {
    /// Get a value by key. Returns `None` if the key does not exist.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;

    /// Insert or update a key-value pair.
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StorageError>;

    /// Delete a key. No-op if the key does not exist.
    fn delete(&mut self, key: &[u8]) -> Result<(), StorageError>;

    /// Returns true if the key exists in the store.
    fn contains(&self, key: &[u8]) -> Result<bool, StorageError> {
        Ok(self.get(key)?.is_some())
    }

    /// Compute the current state root hash over all stored key-value pairs.
    fn state_root(&self) -> Hash;

    /// Create a snapshot that can be restored later.
    fn snapshot(&self) -> Box<dyn StateStore>;

    /// Number of entries in the store.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
