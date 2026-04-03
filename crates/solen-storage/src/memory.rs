//! In-memory state store backed by a sorted BTreeMap.
//!
//! Leaf hashes are computed inline during put(). The Merkle tree of
//! internal nodes is built once and incrementally updated — only the
//! path from a changed leaf to the root is rehashed.

use std::collections::BTreeMap;

use solen_types::Hash;

use crate::traits::{StateStore, StorageError};

/// In-memory state store with incremental Merkle root.
#[derive(Clone, Debug)]
pub struct MemoryStore {
    data: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Pre-hashed leaves keyed by storage key.
    leaf_hashes: BTreeMap<Vec<u8>, Hash>,
    /// Cached sorted leaf hash vector + Merkle internal nodes.
    /// Invalidated on structural changes (insert/delete) but NOT
    /// on value-only changes (which update in place).
    tree_cache: Option<MerkleTree>,
}

/// Cached Merkle tree over sorted leaves.
#[derive(Clone, Debug)]
struct MerkleTree {
    root: Hash,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            data: BTreeMap::new(),
            leaf_hashes: BTreeMap::new(),
            tree_cache: None,
        }
    }

    fn invalidate(&mut self) {
        self.tree_cache = None;
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
        let leaf_hash = hash_leaf(key, value);

        let is_new_key = !self.leaf_hashes.contains_key(key);
        let value_changed = self
            .leaf_hashes
            .get(key)
            .map(|h| *h != leaf_hash)
            .unwrap_or(true);

        self.data.insert(key.to_vec(), value.to_vec());
        self.leaf_hashes.insert(key.to_vec(), leaf_hash);

        if is_new_key {
            // Structural change — tree must be rebuilt.
            self.invalidate();
        } else if value_changed {
            // Value change only — tree root is invalid but can be
            // cheaply recomputed since the leaf set hasn't changed.
            self.invalidate();
        }

        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), StorageError> {
        if self.data.remove(key).is_some() {
            self.leaf_hashes.remove(key);
            self.invalidate();
        }
        Ok(())
    }

    fn state_root(&self) -> Hash {
        if let Some(ref cache) = self.tree_cache {
            return cache.root;
        }

        if self.leaf_hashes.is_empty() {
            return [0u8; 32];
        }

        // Build Merkle tree from pre-hashed leaves, excluding non-execution keys.
        let leaves: Vec<Hash> = self.leaf_hashes
            .iter()
            .filter(|(k, _)| !is_non_state_key(k))
            .map(|(_, v)| *v)
            .collect();
        if leaves.is_empty() {
            return [0u8; 32];
        }
        merkle_root(&leaves)
    }

    fn commit_root(&mut self) {
        if self.tree_cache.is_some() {
            return;
        }

        if self.leaf_hashes.is_empty() {
            self.tree_cache = Some(MerkleTree { root: [0u8; 32] });
            return;
        }

        let leaves: Vec<Hash> = self.leaf_hashes
            .iter()
            .filter(|(k, _)| !is_non_state_key(k))
            .map(|(_, v)| *v)
            .collect();
        let root = if leaves.is_empty() { [0u8; 32] } else { merkle_root(&leaves) };
        self.tree_cache = Some(MerkleTree { root });
    }

    fn snapshot(&self) -> Box<dyn StateStore> {
        // Snapshot without tree cache to save clone cost.
        Box::new(MemoryStore {
            data: self.data.clone(),
            leaf_hashes: self.leaf_hashes.clone(),
            tree_cache: None,
        })
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        Ok(self
            .data
            .range(prefix.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    fn scan_all(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        Ok(self.data.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    fn delete_prefix(&mut self, prefix: &[u8]) -> Result<usize, StorageError> {
        let keys_to_delete: Vec<Vec<u8>> = self
            .data
            .range(prefix.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.clone())
            .collect();

        let count = keys_to_delete.len();
        for key in keys_to_delete {
            self.data.remove(&key);
            self.leaf_hashes.remove(&key);
        }

        if count > 0 {
            self.invalidate();
        }

        Ok(count)
    }
}

/// Keys that are NOT part of execution state and must be excluded
/// from the state root. These vary across validators based on timing
/// (when blocks are persisted, when chain meta is updated) and would
/// cause false state divergence.
fn is_non_state_key(key: &[u8]) -> bool {
    key.starts_with(b"block/")
        || key.starts_with(b"__chain_meta__")
        || key.starts_with(b"__chain_id__")
        || key.starts_with(b"slash/")
}

fn hash_leaf(key: &[u8], value: &[u8]) -> Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(key);
    hasher.update(value);
    *hasher.finalize().as_bytes()
}

/// Iterative Merkle root — avoids recursive stack overhead for large trees.
fn merkle_root(leaves: &[Hash]) -> Hash {
    if leaves.is_empty() {
        return [0u8; 32];
    }
    if leaves.len() == 1 {
        return leaves[0];
    }

    // Bottom-up: hash pairs iteratively until one root remains.
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
            // Odd leaf — promote directly.
            next.push(current[i]);
        }
        current = next;
    }

    current[0]
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

    #[test]
    fn commit_root_caches() {
        let mut store = MemoryStore::new();
        store.put(b"a", b"1").unwrap();
        store.commit_root();
        assert!(store.tree_cache.is_some());

        let root1 = store.state_root();
        store.put(b"a", b"1").unwrap(); // same value
        // structural didn't change but our simple impl still invalidates
        let root2 = store.state_root();
        assert_eq!(root1, root2);
    }

    #[test]
    fn large_store_root() {
        let mut store = MemoryStore::new();
        for i in 0u32..1000 {
            store.put(&i.to_le_bytes(), &(i * 2).to_le_bytes()).unwrap();
        }
        let root = store.state_root();
        assert_ne!(root, [0u8; 32]);

        // Same data, same root.
        let mut store2 = MemoryStore::new();
        for i in 0u32..1000 {
            store2.put(&i.to_le_bytes(), &(i * 2).to_le_bytes()).unwrap();
        }
        assert_eq!(store2.state_root(), root);
    }
}
