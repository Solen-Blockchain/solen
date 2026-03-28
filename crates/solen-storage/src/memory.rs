//! In-memory state store backed by a sorted BTreeMap.
//!
//! The state root is computed as a Merkle tree over sorted key-value pairs
//! using BLAKE3. This is suitable for development, testing, and single-node
//! devnets. Production deployments will use a persistent backend.

use std::collections::BTreeMap;

use solen_types::Hash;

use crate::traits::{StateStore, StorageError};

/// In-memory state store with BLAKE3 Merkle root.
#[derive(Clone, Debug)]
pub struct MemoryStore {
    data: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            data: BTreeMap::new(),
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StateStore for MemoryStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self.data.get(key).cloned())
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.data.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), StorageError> {
        self.data.remove(key);
        Ok(())
    }

    fn state_root(&self) -> Hash {
        if self.data.is_empty() {
            return [0u8; 32];
        }

        // Hash each key-value pair into a leaf.
        let leaves: Vec<Hash> = self
            .data
            .iter()
            .map(|(k, v)| {
                let mut hasher = blake3::Hasher::new();
                hasher.update(k);
                hasher.update(v);
                *hasher.finalize().as_bytes()
            })
            .collect();

        merkle_root(&leaves)
    }

    fn snapshot(&self) -> Box<dyn StateStore> {
        Box::new(self.clone())
    }

    fn len(&self) -> usize {
        self.data.len()
    }
}

/// Compute a binary Merkle root from a slice of leaf hashes.
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

    #[test]
    fn empty_store_has_zero_root() {
        let store = MemoryStore::new();
        assert_eq!(store.state_root(), [0u8; 32]);
        assert!(store.is_empty());
    }

    #[test]
    fn put_get_delete() {
        let mut store = MemoryStore::new();
        store.put(b"alice", b"100").unwrap();
        assert_eq!(store.get(b"alice").unwrap(), Some(b"100".to_vec()));
        assert_eq!(store.len(), 1);

        store.delete(b"alice").unwrap();
        assert_eq!(store.get(b"alice").unwrap(), None);
        assert!(store.is_empty());
    }

    #[test]
    fn state_root_changes_on_mutation() {
        let mut store = MemoryStore::new();
        store.put(b"key1", b"val1").unwrap();
        let root1 = store.state_root();

        store.put(b"key2", b"val2").unwrap();
        let root2 = store.state_root();

        assert_ne!(root1, root2);
        assert_ne!(root1, [0u8; 32]);
    }

    #[test]
    fn state_root_is_deterministic() {
        let mut a = MemoryStore::new();
        a.put(b"x", b"1").unwrap();
        a.put(b"y", b"2").unwrap();

        let mut b = MemoryStore::new();
        // Insert in different order — BTreeMap sorts, so root should match.
        b.put(b"y", b"2").unwrap();
        b.put(b"x", b"1").unwrap();

        assert_eq!(a.state_root(), b.state_root());
    }

    #[test]
    fn snapshot_is_independent() {
        let mut store = MemoryStore::new();
        store.put(b"k", b"v").unwrap();
        let snap = store.snapshot();

        store.put(b"k", b"changed").unwrap();

        assert_eq!(snap.get(b"k").unwrap(), Some(b"v".to_vec()));
        assert_eq!(store.get(b"k").unwrap(), Some(b"changed".to_vec()));
    }
}
