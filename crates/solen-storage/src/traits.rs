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

/// A rollback point for a single operation within a block.
///
/// When a block executes against a buffered store (`OverlayStore`), the
/// underlying DB is never mutated mid-block, so a savepoint is just a cheap
/// clone of the staged delta. For non-buffered stores it falls back to a full
/// snapshot.
pub enum Savepoint {
    /// Cheap delta clone: staged changes at savepoint time
    /// (`Some(v)` = staged write, `None` = staged delete).
    Delta(std::collections::HashMap<Vec<u8>, Option<Vec<u8>>>),
    /// Full snapshot fallback for non-buffered stores.
    Full(Box<dyn StateStore>),
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

    /// Apply a set of staged changes (`Some(v)` = put, `None` = delete) to the
    /// store atomically, so a crash mid-apply leaves the store either fully
    /// before or fully after this batch — never a partial block. `sync` fsyncs
    /// the write-ahead log for durability across power loss.
    ///
    /// Default implementation applies the changes one by one (NOT atomic);
    /// crash-relevant backends (RocksDB) override this with a real write batch.
    fn apply_batch_atomic(
        &mut self,
        changes: &std::collections::HashMap<Vec<u8>, Option<Vec<u8>>>,
        _sync: bool,
    ) -> Result<(), StorageError> {
        for (k, v) in changes {
            match v {
                Some(val) => self.put(k, val)?,
                None => self.delete(k)?,
            }
        }
        Ok(())
    }

    /// Take a savepoint for cheap rollback of a single operation. Buffered
    /// stores override this to return a `Savepoint::Delta` (a clone of the
    /// staged changes); the default takes a `Savepoint::Full` snapshot.
    fn savepoint(&self) -> Savepoint {
        Savepoint::Full(self.snapshot())
    }

    /// Restore the store to a previously taken savepoint (rolling back a failed
    /// operation). Buffered stores override the `Delta` arm; the default handles
    /// `Full` by reverting every key to the snapshot (delete keys created since,
    /// re-put snapshot values).
    fn restore_savepoint(&mut self, sp: Savepoint) {
        match sp {
            Savepoint::Full(snap) => {
                let snap_entries = snap.scan_all().unwrap_or_default();
                let snap_keys: std::collections::HashSet<&[u8]> =
                    snap_entries.iter().map(|(k, _)| k.as_slice()).collect();
                if let Ok(current) = self.scan_all() {
                    for (k, _) in &current {
                        if !snap_keys.contains(k.as_slice()) {
                            let _ = self.delete(k);
                        }
                    }
                }
                for (k, v) in &snap_entries {
                    let _ = self.put(k, v);
                }
            }
            // Buffered stores override this; a non-buffered store should never
            // be handed a Delta savepoint.
            Savepoint::Delta(_) => {}
        }
    }

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
