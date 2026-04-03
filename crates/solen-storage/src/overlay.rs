//! Copy-on-write overlay store.
//!
//! Wraps a read-only reference to a base store and captures writes in memory.
//! Used for simulation without copying the entire database.

use std::collections::{BTreeMap, HashSet};

use solen_types::Hash;

use crate::traits::{StateStore, StorageError};

/// A copy-on-write overlay that reads from a base store and writes to memory.
pub struct OverlayStore<'a> {
    base: &'a dyn StateStore,
    writes: BTreeMap<Vec<u8>, Vec<u8>>,
    deletes: HashSet<Vec<u8>>,
}

impl<'a> OverlayStore<'a> {
    pub fn new(base: &'a dyn StateStore) -> Self {
        Self {
            base,
            writes: BTreeMap::new(),
            deletes: HashSet::new(),
        }
    }
}

impl<'a> StateStore for OverlayStore<'a> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        // Check deletes first.
        if self.deletes.contains(key) {
            return Ok(None);
        }
        // Check local writes.
        if let Some(val) = self.writes.get(key) {
            return Ok(Some(val.clone()));
        }
        // Fall through to base.
        self.base.get(key)
    }

    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.deletes.remove(key);
        self.writes.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), StorageError> {
        self.writes.remove(key);
        self.deletes.insert(key.to_vec());
        Ok(())
    }

    fn state_root(&self) -> Hash {
        // For simulation purposes, return base root (exact root not needed).
        self.base.state_root()
    }

    fn snapshot(&self) -> Box<dyn StateStore> {
        // Nested snapshot — just clone the overlay into a MemoryStore.
        let mut mem = crate::memory::MemoryStore::new();
        // This shouldn't be called in practice during simulation.
        for (k, v) in &self.writes {
            let _ = mem.put(k, v);
        }
        Box::new(mem)
    }

    fn len(&self) -> usize {
        // Approximate.
        self.base.len() + self.writes.len()
    }

    fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        let mut results: std::collections::BTreeMap<Vec<u8>, Vec<u8>> = std::collections::BTreeMap::new();
        // Start with base.
        for (k, v) in self.base.scan_prefix(prefix)? {
            if !self.deletes.contains(&k) {
                results.insert(k, v);
            }
        }
        // Overlay writes on top.
        for (k, v) in self.writes.range(prefix.to_vec()..) {
            if !k.starts_with(prefix) { break; }
            results.insert(k.clone(), v.clone());
        }
        Ok(results.into_iter().collect())
    }

    fn delete_prefix(&mut self, prefix: &[u8]) -> Result<usize, StorageError> {
        let keys: Vec<_> = self
            .writes
            .range(prefix.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.clone())
            .collect();
        let count = keys.len();
        for k in keys {
            self.writes.remove(&k);
            self.deletes.insert(k);
        }
        Ok(count)
    }
}
