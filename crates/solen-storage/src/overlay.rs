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

    /// Consume the overlay and return its staged changes as a single change set
    /// (`Some(v)` = write, `None` = delete), suitable for
    /// `StateStore::apply_batch_atomic` against the base store.
    pub fn into_changes(self) -> std::collections::HashMap<Vec<u8>, Option<Vec<u8>>> {
        let mut changes: std::collections::HashMap<Vec<u8>, Option<Vec<u8>>> =
            std::collections::HashMap::with_capacity(self.writes.len() + self.deletes.len());
        for (k, v) in self.writes {
            changes.insert(k, Some(v));
        }
        for k in self.deletes {
            changes.insert(k, None);
        }
        changes
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

    /// Cheap savepoint: clone only the staged delta. The base store is never
    /// mutated while executing against the overlay, so this fully captures the
    /// rollback state for one operation.
    fn savepoint(&self) -> crate::traits::Savepoint {
        let mut delta: std::collections::HashMap<Vec<u8>, Option<Vec<u8>>> =
            std::collections::HashMap::with_capacity(self.writes.len() + self.deletes.len());
        for (k, v) in &self.writes {
            delta.insert(k.clone(), Some(v.clone()));
        }
        for k in &self.deletes {
            delta.insert(k.clone(), None);
        }
        crate::traits::Savepoint::Delta(delta)
    }

    fn restore_savepoint(&mut self, sp: crate::traits::Savepoint) {
        if let crate::traits::Savepoint::Delta(delta) = sp {
            self.writes.clear();
            self.deletes.clear();
            for (k, v) in delta {
                match v {
                    Some(val) => {
                        self.writes.insert(k, val);
                    }
                    None => {
                        self.deletes.insert(k);
                    }
                }
            }
        }
        // A Full savepoint on an overlay is unexpected; ignore.
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

    fn scan_all(&self) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
        let mut results: std::collections::BTreeMap<Vec<u8>, Vec<u8>> = std::collections::BTreeMap::new();
        for (k, v) in self.base.scan_all()? {
            if !self.deletes.contains(&k) {
                results.insert(k, v);
            }
        }
        for (k, v) in &self.writes {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;

    #[test]
    fn savepoint_rolls_back_only_the_op_delta() {
        let mut base = MemoryStore::new();
        base.put(b"x", b"base").unwrap();
        let mut ov = OverlayStore::new(&base);

        ov.put(b"a", b"1").unwrap(); // op 1 (kept)
        let sp = ov.savepoint();
        ov.put(b"b", b"2").unwrap(); // op 2 (rolled back)
        ov.delete(b"x").unwrap();
        ov.put(b"a", b"changed").unwrap();

        ov.restore_savepoint(sp);

        assert_eq!(ov.get(b"a").unwrap(), Some(b"1".to_vec()), "op1 write survives");
        assert_eq!(ov.get(b"b").unwrap(), None, "op2 write reverted");
        assert_eq!(ov.get(b"x").unwrap(), Some(b"base".to_vec()), "op2 delete reverted");
    }

    #[test]
    fn into_changes_captures_writes_and_deletes() {
        let mut base = MemoryStore::new();
        base.put(b"keep", b"v").unwrap();
        let mut ov = OverlayStore::new(&base);
        ov.put(b"w", b"1").unwrap();
        ov.delete(b"keep").unwrap();
        let changes = ov.into_changes();
        assert_eq!(changes.get(b"w".as_ref()), Some(&Some(b"1".to_vec())));
        assert_eq!(changes.get(b"keep".as_ref()), Some(&None));
    }
}
