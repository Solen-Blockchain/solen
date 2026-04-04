//! RocksDB-backed persistent state store.
//!
//! Uses a single column family with sorted keys for deterministic iteration.
//! The state root is computed as a BLAKE3 Merkle tree over all key-value pairs,
//! matching the MemoryStore implementation for compatibility.

use std::path::Path;

use rocksdb::{checkpoint::Checkpoint, IteratorMode, Options, DB};
use solen_types::Hash;
use tracing::info;

use crate::traits::{StateStore, StorageError};

/// Persistent state store backed by RocksDB.
pub struct RocksStore {
    db: DB,
}

impl RocksStore {
    /// Open or create a RocksDB database at the given path.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.set_compression_type(rocksdb::DBCompressionType::Lz4);

        let db = DB::open(&opts, path).map_err(|e| StorageError::Backend(e.to_string()))?;

        info!(path = %path.display(), "RocksDB opened");

        Ok(Self { db })
    }

    /// Create a native RocksDB checkpoint (hard-linked, near-instant).
    /// Returns a read-only RocksStore backed by the checkpoint.
    /// The checkpoint directory is cleaned up when the returned store is dropped.
    pub fn checkpoint(&self) -> Result<CheckpointStore, StorageError> {
        static CHECKPOINT_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = CHECKPOINT_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("solen-checkpoint-{}-{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&dir);

        let cp = Checkpoint::new(&self.db)
            .map_err(|e| StorageError::Backend(format!("checkpoint create: {e}")))?;
        cp.create_checkpoint(&dir)
            .map_err(|e| StorageError::Backend(format!("checkpoint write: {e}")))?;

        let mut opts = Options::default();
        opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
        let db = DB::open_for_read_only(&opts, &dir, false)
            .map_err(|e| StorageError::Backend(format!("checkpoint open: {e}")))?;

        info!(path = %dir.display(), "RocksDB checkpoint created");

        Ok(CheckpointStore { db, dir })
    }
}

/// A read-only RocksDB checkpoint that cleans up on drop.
pub struct CheckpointStore {
    db: DB,
    dir: std::path::PathBuf,
}

impl Drop for CheckpointStore {
    fn drop(&mut self) {
        // Close DB first (happens implicitly), then remove dir.
        let dir = self.dir.clone();
        // DB must be dropped before we can remove the directory.
        // Since drop order in a struct is field order, db drops first.
        let _ = std::fs::remove_dir_all(&dir);
    }
}

impl StateStore for CheckpointStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        self.db.get(key).map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn put(&mut self, _key: &[u8], _value: &[u8]) -> Result<(), StorageError> {
        Err(StorageError::Backend("checkpoint is read-only".into()))
    }

    fn delete(&mut self, _key: &[u8]) -> Result<(), StorageError> {
        Err(StorageError::Backend("checkpoint is read-only".into()))
    }

    fn state_root(&self) -> Hash {
        let mut leaves: Vec<Hash> = Vec::new();
        for item in self.db.iterator(IteratorMode::Start) {
            let (k, v) = match item {
                Ok(kv) => kv,
                Err(_) => continue, // Skip corrupted entries.
            };
            if k.starts_with(b"block/") || k.starts_with(b"__chain_meta__") || k.starts_with(b"__chain_id__") || k.starts_with(b"slash/") || k.starts_with(b"source/") || k.starts_with(b"__finalized_checkpoint__") {
                continue;
            }
            let mut hasher = blake3::Hasher::new();
            hasher.update(&k);
            hasher.update(&v);
            leaves.push(*hasher.finalize().as_bytes());
        }
        merkle_root(&leaves)
    }

    fn snapshot(&self) -> Box<dyn StateStore> {
        // Checkpoint of a checkpoint — just copy to memory.
        let mut mem = crate::memory::MemoryStore::new();
        for item in self.db.iterator(IteratorMode::Start) {
            match item {
                Ok((k, v)) => { let _ = mem.put(&k, &v); }
                Err(e) => {
                    tracing::error!(error = %e, "RocksDB iteration error in checkpoint snapshot — skipping entry");
                }
            }
        }
        Box::new(mem)
    }

    fn len(&self) -> usize {
        self.db.iterator(IteratorMode::Start).count()
    }

    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        Ok(self.db
            .prefix_iterator(prefix)
            .filter_map(|item| item.ok())
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect())
    }

    fn scan_all(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        Ok(self.db
            .iterator(IteratorMode::Start)
            .filter_map(|item| item.ok())
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect())
    }
}

impl StateStore for RocksStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        self.db
            .get(key)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.db
            .put(key, value)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), StorageError> {
        self.db
            .delete(key)
            .map_err(|e| StorageError::Backend(e.to_string()))
    }

    fn state_root(&self) -> Hash {
        let mut leaves: Vec<Hash> = Vec::new();

        let iter = self.db.iterator(IteratorMode::Start);
        for item in iter {
            let (k, v) = match item {
                Ok(kv) => kv,
                Err(e) => {
                    // Fail loudly on RocksDB iteration errors. Silent skip would
                    // produce an incomplete merkle root, causing state divergence
                    // between nodes with partial disk corruption.
                    tracing::error!(error = %e, "RocksDB iteration error during state_root — aborting");
                    panic!("RocksDB iteration error in state_root: {e}");
                }
            };
            // Exclude non-execution keys from the state root.
            // Block storage and chain metadata differ across validators
            // based on timing, which would cause false state divergence.
            if k.starts_with(b"block/") || k.starts_with(b"__chain_meta__") || k.starts_with(b"__chain_id__") || k.starts_with(b"slash/") || k.starts_with(b"source/") || k.starts_with(b"__finalized_checkpoint__") {
                continue;
            }
            let mut hasher = blake3::Hasher::new();
            hasher.update(&k);
            hasher.update(&v);
            leaves.push(*hasher.finalize().as_bytes());
        }

        merkle_root(&leaves)
    }

    fn snapshot(&self) -> Box<dyn StateStore> {
        // Use native RocksDB checkpoint (instant, hard-linked, read-only).
        if let Ok(cp) = self.checkpoint() {
            return Box::new(cp);
        }

        // Fallback: copy to memory.
        let mut mem = crate::memory::MemoryStore::new();
        let iter = self.db.iterator(IteratorMode::Start);
        for item in iter {
            match item {
                Ok((k, v)) => { let _ = mem.put(&k, &v); }
                Err(e) => {
                    tracing::error!(error = %e, "RocksDB iteration error in snapshot fallback — skipping entry");
                }
            }
        }
        Box::new(mem)
    }

    fn len(&self) -> usize {
        self.db.iterator(IteratorMode::Start).count()
    }

    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        Ok(self
            .db
            .prefix_iterator(prefix)
            .filter_map(|item| item.ok())
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect())
    }

    fn scan_all(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        Ok(self
            .db
            .iterator(IteratorMode::Start)
            .filter_map(|item| item.ok())
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect())
    }

    fn delete_prefix(&mut self, prefix: &[u8]) -> Result<usize, StorageError> {
        let keys_to_delete: Vec<Vec<u8>> = self
            .db
            .prefix_iterator(prefix)
            .filter_map(|item| item.ok())
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.to_vec())
            .collect();

        let count = keys_to_delete.len();
        for key in keys_to_delete {
            self.db
                .delete(&key)
                .map_err(|e| StorageError::Backend(e.to_string()))?;
        }

        Ok(count)
    }
}

/// Iterative Merkle root — MUST match MemoryStore's algorithm exactly.
/// Pairwise bottom-up hashing with odd-leaf promotion.
fn merkle_root(leaves: &[Hash]) -> Hash {
    if leaves.is_empty() {
        return [0u8; 32];
    }
    if leaves.len() == 1 {
        return leaves[0];
    }

    let mut current: Vec<Hash> = leaves.to_vec();
    while current.len() > 1 {
        let mut next = Vec::with_capacity((current.len() + 1) / 2);
        let mut i = 0;
        while i + 1 < current.len() {
            let mut hasher = blake3::Hasher::new();
            hasher.update(&current[i]);
            hasher.update(&current[i + 1]);
            next.push(*hasher.finalize().as_bytes());
            i += 2;
        }
        if i < current.len() {
            next.push(current[i]); // Odd leaf promoted.
        }
        current = next;
    }

    current[0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_store() -> (RocksStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let store = RocksStore::open(dir.path()).unwrap();
        (store, dir)
    }

    #[test]
    fn put_get_delete() {
        let (mut store, _dir) = temp_store();
        store.put(b"key1", b"val1").unwrap();
        assert_eq!(store.get(b"key1").unwrap(), Some(b"val1".to_vec()));

        store.delete(b"key1").unwrap();
        assert_eq!(store.get(b"key1").unwrap(), None);
    }

    #[test]
    fn state_root_matches_memory_store() {
        let (mut rocks, _dir) = temp_store();
        let mut mem = crate::memory::MemoryStore::new();

        rocks.put(b"a", b"1").unwrap();
        rocks.put(b"b", b"2").unwrap();
        mem.put(b"a", b"1").unwrap();
        mem.put(b"b", b"2").unwrap();

        assert_eq!(rocks.state_root(), mem.state_root());
    }

    #[test]
    fn persistence_across_reopen() {
        let dir = TempDir::new().unwrap();

        {
            let mut store = RocksStore::open(dir.path()).unwrap();
            store.put(b"persist", b"value").unwrap();
        }

        {
            let store = RocksStore::open(dir.path()).unwrap();
            assert_eq!(store.get(b"persist").unwrap(), Some(b"value".to_vec()));
        }
    }

    #[test]
    fn snapshot_is_independent() {
        let (mut store, _dir) = temp_store();
        store.put(b"k", b"v").unwrap();
        let snap = store.snapshot();

        store.put(b"k", b"changed").unwrap();

        assert_eq!(snap.get(b"k").unwrap(), Some(b"v".to_vec()));
        assert_eq!(store.get(b"k").unwrap(), Some(b"changed".to_vec()));
    }
}
