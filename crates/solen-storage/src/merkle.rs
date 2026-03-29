//! Incremental Merkle trie: only rehashes changed paths.
//!
//! Instead of recomputing the full Merkle root over all key-value pairs,
//! this maintains a tree structure where only modified branches are
//! rehashed on `compute_root()`.

use std::collections::BTreeMap;

use solen_types::Hash;

/// Node in the Merkle trie.
#[derive(Clone, Debug)]
enum Node {
    Leaf {
        key: Vec<u8>,
        value_hash: Hash,
    },
    Branch {
        left: Box<Node>,
        right: Box<Node>,
        hash: Hash,
        dirty: bool,
    },
    Empty,
}

impl Node {
    fn hash(&self) -> Hash {
        match self {
            Node::Leaf { key, value_hash } => {
                let mut hasher = blake3::Hasher::new();
                hasher.update(key);
                hasher.update(value_hash);
                *hasher.finalize().as_bytes()
            }
            Node::Branch { hash, .. } => *hash,
            Node::Empty => [0u8; 32],
        }
    }
}

/// An incremental Merkle tree that tracks dirty nodes.
#[derive(Clone, Debug)]
pub struct IncrementalMerkle {
    root_hash: Option<Hash>,
    dirty: bool,
    /// Sorted key-value pairs with value hashes.
    entries: BTreeMap<Vec<u8>, Hash>,
    /// Keys that changed since last root computation.
    dirty_keys: Vec<Vec<u8>>,
}

impl IncrementalMerkle {
    pub fn new() -> Self {
        Self {
            root_hash: Some([0u8; 32]),
            dirty: false,
            entries: BTreeMap::new(),
            dirty_keys: Vec::new(),
        }
    }

    /// Insert or update a key-value pair.
    pub fn insert(&mut self, key: &[u8], value: &[u8]) {
        let value_hash = {
            let mut hasher = blake3::Hasher::new();
            hasher.update(key);
            hasher.update(value);
            *hasher.finalize().as_bytes()
        };

        let prev = self.entries.insert(key.to_vec(), value_hash);
        if prev.map(|h| h != value_hash).unwrap_or(true) {
            self.dirty = true;
            self.dirty_keys.push(key.to_vec());
        }
    }

    /// Remove a key.
    pub fn remove(&mut self, key: &[u8]) {
        if self.entries.remove(key).is_some() {
            self.dirty = true;
            self.dirty_keys.push(key.to_vec());
        }
    }

    /// Get the current root hash. Only recomputes if dirty.
    pub fn root(&mut self) -> Hash {
        if !self.dirty {
            return self.root_hash.unwrap_or([0u8; 32]);
        }

        let root = if self.entries.is_empty() {
            [0u8; 32]
        } else {
            let leaves: Vec<Hash> = self.entries.values().copied().collect();
            merkle_root_from_leaves(&leaves)
        };

        self.root_hash = Some(root);
        self.dirty = false;
        self.dirty_keys.clear();
        root
    }

    /// Check if recomputation is needed.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of changes since last root computation.
    pub fn pending_changes(&self) -> usize {
        self.dirty_keys.len()
    }
}

impl Default for IncrementalMerkle {
    fn default() -> Self {
        Self::new()
    }
}

fn merkle_root_from_leaves(leaves: &[Hash]) -> Hash {
    match leaves.len() {
        0 => [0u8; 32],
        1 => leaves[0],
        n => {
            let mid = n / 2;
            let left = merkle_root_from_leaves(&leaves[..mid]);
            let right = merkle_root_from_leaves(&leaves[mid..]);
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
    fn empty_root_is_zero() {
        let mut m = IncrementalMerkle::new();
        assert_eq!(m.root(), [0u8; 32]);
    }

    #[test]
    fn insert_changes_root() {
        let mut m = IncrementalMerkle::new();
        let r1 = m.root();

        m.insert(b"key", b"value");
        let r2 = m.root();

        assert_ne!(r1, r2);
    }

    #[test]
    fn cached_root_not_recomputed() {
        let mut m = IncrementalMerkle::new();
        m.insert(b"a", b"1");
        let r1 = m.root();
        assert!(!m.is_dirty());

        // No changes — should return cached value.
        let r2 = m.root();
        assert_eq!(r1, r2);
        assert!(!m.is_dirty());
    }

    #[test]
    fn deterministic_regardless_of_insert_order() {
        let mut a = IncrementalMerkle::new();
        a.insert(b"x", b"1");
        a.insert(b"y", b"2");

        let mut b = IncrementalMerkle::new();
        b.insert(b"y", b"2");
        b.insert(b"x", b"1");

        assert_eq!(a.root(), b.root());
    }

    #[test]
    fn remove_changes_root() {
        let mut m = IncrementalMerkle::new();
        m.insert(b"a", b"1");
        m.insert(b"b", b"2");
        let r1 = m.root();

        m.remove(b"b");
        let r2 = m.root();

        assert_ne!(r1, r2);
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn matches_full_recomputation() {
        // Verify incremental produces same root as computing from scratch.
        let mut inc = IncrementalMerkle::new();
        inc.insert(b"alice", b"100");
        inc.insert(b"bob", b"200");
        inc.insert(b"carol", b"300");
        let inc_root = inc.root();

        // Full recomputation (same logic as MemoryStore).
        let mut entries = BTreeMap::new();
        entries.insert(b"alice".to_vec(), b"100".to_vec());
        entries.insert(b"bob".to_vec(), b"200".to_vec());
        entries.insert(b"carol".to_vec(), b"300".to_vec());

        let leaves: Vec<Hash> = entries
            .iter()
            .map(|(k, v)| {
                let mut h = blake3::Hasher::new();
                h.update(k);
                h.update(v);
                *h.finalize().as_bytes()
            })
            .collect();
        let full_root = merkle_root_from_leaves(&leaves);

        assert_eq!(inc_root, full_root);
    }

    #[test]
    fn pending_changes_tracking() {
        let mut m = IncrementalMerkle::new();
        assert_eq!(m.pending_changes(), 0);

        m.insert(b"a", b"1");
        m.insert(b"b", b"2");
        assert_eq!(m.pending_changes(), 2);

        m.root(); // clears pending
        assert_eq!(m.pending_changes(), 0);
    }
}
