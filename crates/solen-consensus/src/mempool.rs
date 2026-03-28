//! Transaction mempool: collects pending user operations for inclusion in blocks.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use solen_types::transaction::UserOperation;

/// Thread-safe mempool for pending user operations.
#[derive(Clone)]
pub struct Mempool {
    inner: Arc<Mutex<VecDeque<UserOperation>>>,
    max_size: usize,
}

impl Mempool {
    pub fn new(max_size: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            max_size,
        }
    }

    /// Add an operation to the mempool. Returns false if the pool is full.
    pub fn submit(&self, op: UserOperation) -> bool {
        let mut pool = self.inner.lock().unwrap();
        if pool.len() >= self.max_size {
            return false;
        }
        pool.push_back(op);
        true
    }

    /// Drain up to `limit` operations from the front of the pool.
    pub fn drain(&self, limit: usize) -> Vec<UserOperation> {
        let mut pool = self.inner.lock().unwrap();
        let n = limit.min(pool.len());
        pool.drain(..n).collect()
    }

    /// Number of pending operations.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solen_types::transaction::Action;

    fn dummy_op(nonce: u64) -> UserOperation {
        UserOperation {
            sender: [0u8; 32],
            nonce,
            actions: vec![Action::Transfer {
                to: [1u8; 32],
                amount: 10,
            }],
            max_fee: 100,
            signature: vec![],
        }
    }

    #[test]
    fn submit_and_drain() {
        let pool = Mempool::new(100);
        pool.submit(dummy_op(0));
        pool.submit(dummy_op(1));
        assert_eq!(pool.len(), 2);

        let ops = pool.drain(1);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].nonce, 0);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn respects_max_size() {
        let pool = Mempool::new(2);
        assert!(pool.submit(dummy_op(0)));
        assert!(pool.submit(dummy_op(1)));
        assert!(!pool.submit(dummy_op(2)));
    }
}
