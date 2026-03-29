//! RocksDB-backed persistent state store.
//!
//! Uses a single column family with sorted keys for deterministic iteration.
//! The state root is computed as a BLAKE3 Merkle tree over all key-value pairs,
//! matching the MemoryStore implementation for compatibility.

use std::path::Path;

use rocksdb::{IteratorMode, Options, DB};
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
            let (k, v) = item.unwrap();
            let mut hasher = blake3::Hasher::new();
            hasher.update(&k);
            hasher.update(&v);
            leaves.push(*hasher.finalize().as_bytes());
        }

        merkle_root(&leaves)
    }

    fn snapshot(&self) -> Box<dyn StateStore> {
        // Create an in-memory copy for simulation/snapshot purposes.
        let mut mem = crate::memory::MemoryStore::new();
        let iter = self.db.iterator(IteratorMode::Start);
        for item in iter {
            let (k, v) = item.unwrap();
            mem.put(&k, &v).unwrap();
        }
        Box::new(mem)
    }

    fn len(&self) -> usize {
        self.db.iterator(IteratorMode::Start).count()
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

/// Compute a binary Merkle root from leaf hashes (same algorithm as MemoryStore).
fn merkle_root(leaves: &[Hash]) -> Hash {
    match leaves.len() {
        0 => [0u8; 32],
        1 => leaves[0],
        n => {
            let mid = n / 2;
            let left = merkle_root(&leaves[..mid]);
            let right = merkle_root(&leaves[mid..]);
            let mut hasher = blake3::Hasher::new();
            hasher.update(&left);
            hasher.update(&right);
            *hasher.finalize().as_bytes()
        }
    }
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
