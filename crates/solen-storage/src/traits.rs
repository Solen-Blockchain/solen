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

    /// Cache the current state root. Call after a batch of writes to avoid
    /// recomputing on subsequent `state_root()` calls.
    fn commit_root(&mut self) {
        // Default: no-op. Backends with caching override this.
    }

    /// Create a snapshot that can be restored later.
    fn snapshot(&self) -> Box<dyn StateStore>;

    /// Number of entries in the store.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Delete all keys matching a prefix.
    /// Default implementation does nothing — backends should override.
    fn delete_prefix(&mut self, _prefix: &[u8]) -> Result<usize, StorageError> {
        Ok(0)
    }

    /// Iterate all key-value pairs whose keys start with the given prefix.
    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError>;

    /// Iterate all key-value pairs in the store.
    fn scan_all(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError>;

    /// Delete all entries in the store.
    fn clear(&mut self) -> Result<(), StorageError> {
        let keys: Vec<Vec<u8>> = self.scan_all()?.into_iter().map(|(k, _)| k).collect();
        for k in keys {
            self.delete(&k)?;
        }
        Ok(())
    }

    /// Create a writable in-memory snapshot for trial execution.
    /// Unlike `snapshot()` which may be read-only, this always returns a
    /// fully writable store suitable for speculative execution.
    fn writable_snapshot(&self) -> Box<dyn StateStore> {
        let mut mem = crate::memory::MemoryStore::new();
        if let Ok(entries) = self.scan_all() {
            for (k, v) in entries {
                let _ = mem.put(&k, &v);
            }
        }
        Box::new(mem)
    }
}
