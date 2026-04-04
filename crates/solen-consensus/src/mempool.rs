//! Transaction mempool: collects pending user operations for inclusion in blocks.
//!
//! Features:
//! - Duplicate detection by (sender, nonce)
//! - Fee-based priority ordering (higher max_fee first)
//! - Configurable max size

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use solen_types::transaction::UserOperation;

/// Maximum pending operations per sender to prevent single-sender spam.
const MAX_OPS_PER_SENDER: usize = 16;

/// A mempool entry wrapping a UserOperation with ordering by fee (descending).
#[derive(Clone, Debug)]
struct MempoolEntry {
    op: UserOperation,
}

impl PartialEq for MempoolEntry {
    fn eq(&self, other: &Self) -> bool {
        self.op.sender == other.op.sender && self.op.nonce == other.op.nonce
    }
}

impl Eq for MempoolEntry {}

impl PartialOrd for MempoolEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MempoolEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Higher fee first (reverse order).
        other
            .op
            .max_fee
            .cmp(&self.op.max_fee)
            // Break ties by sender + nonce for determinism.
            .then_with(|| self.op.sender.cmp(&other.op.sender))
            .then_with(|| self.op.nonce.cmp(&other.op.nonce))
    }
}

/// Dedup key: (sender, nonce).
type DedupKey = ([u8; 32], u64);

/// Thread-safe mempool for pending user operations.
#[derive(Clone)]
pub struct Mempool {
    inner: Arc<Mutex<MempoolInner>>,
    max_size: usize,
}

struct MempoolInner {
    entries: BTreeSet<MempoolEntry>,
    seen: HashSet<DedupKey>,
    sender_counts: HashMap<[u8; 32], usize>,
}

impl Mempool {
    pub fn new(max_size: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MempoolInner {
                entries: BTreeSet::new(),
                seen: HashSet::new(),
                sender_counts: HashMap::new(),
            })),
            max_size,
        }
    }

    /// Maximum serialized size of a single operation (256 KB).
    /// Prevents memory exhaustion from operations with huge code or args.
    const MAX_OP_SIZE: usize = 256 * 1024;

    /// Add an operation to the mempool. Returns false if pool is full, duplicate,
    /// or if the operation is a system-reserved intent operation.
    pub fn submit(&self, op: UserOperation) -> bool {
        // Reject system-authorized intent operations — these are injected by the
        // block proposer only, never accepted from external sources.
        if op.signature == [0xFF] {
            tracing::warn!(
                sender = ?&op.sender[..4],
                "rejected [0xFF] system signature from external submission"
            );
            return false;
        }

        // Reject oversized operations (prevent memory exhaustion).
        let op_size: usize = op.signature.len()
            + op.actions.iter().map(|a| match a {
                solen_types::transaction::Action::Deploy { code, .. } => code.len() + 32,
                solen_types::transaction::Action::Call { args, method, .. } => args.len() + method.len() + 32,
                _ => 64,
            }).sum::<usize>();
        if op_size > Self::MAX_OP_SIZE {
            return false;
        }

        let mut pool = self.inner.lock().unwrap();
        if pool.entries.len() >= self.max_size {
            return false;
        }

        // Per-sender limit to prevent single-sender spam.
        let sender_count = pool.sender_counts.get(&op.sender).copied().unwrap_or(0);
        if sender_count >= MAX_OPS_PER_SENDER {
            return false;
        }

        let key: DedupKey = (op.sender, op.nonce);
        if pool.seen.contains(&key) {
            return false; // Duplicate sender+nonce.
        }

        let sender = op.sender;
        pool.seen.insert(key);
        pool.entries.insert(MempoolEntry { op });
        *pool.sender_counts.entry(sender).or_insert(0) += 1;
        true
    }

    /// Drain up to `limit` operations, highest fee first.
    pub fn drain(&self, limit: usize) -> Vec<UserOperation> {
        let mut pool = self.inner.lock().unwrap();
        let n = limit.min(pool.entries.len());
        let mut ops = Vec::with_capacity(n);

        for _ in 0..n {
            if let Some(entry) = pool.entries.pop_first() {
                pool.seen.remove(&(entry.op.sender, entry.op.nonce));
                if let Some(count) = pool.sender_counts.get_mut(&entry.op.sender) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        pool.sender_counts.remove(&entry.op.sender);
                    }
                }
                ops.push(entry.op);
            }
        }

        ops
    }

    /// Number of pending operations.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solen_types::transaction::Action;

    fn dummy_op(sender_byte: u8, nonce: u64, fee: u128) -> UserOperation {
        let mut sender = [0u8; 32];
        sender[0] = sender_byte;
        UserOperation {
            sender,
            nonce,
            actions: vec![Action::Transfer {
                to: [1u8; 32],
                amount: 10,
            }],
            max_fee: fee,
            signature: vec![],
        }
    }

    #[test]
    fn submit_and_drain() {
        let pool = Mempool::new(100);
        pool.submit(dummy_op(1, 0, 100));
        pool.submit(dummy_op(2, 0, 200));
        assert_eq!(pool.len(), 2);

        let ops = pool.drain(1);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].max_fee, 200); // Higher fee drained first.
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn rejects_duplicates() {
        let pool = Mempool::new(100);
        assert!(pool.submit(dummy_op(1, 0, 100)));
        assert!(!pool.submit(dummy_op(1, 0, 200))); // Same sender+nonce.
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn respects_max_size() {
        let pool = Mempool::new(2);
        assert!(pool.submit(dummy_op(1, 0, 100)));
        assert!(pool.submit(dummy_op(2, 0, 100)));
        assert!(!pool.submit(dummy_op(3, 0, 100)));
    }

    #[test]
    fn fee_ordering() {
        let pool = Mempool::new(100);
        pool.submit(dummy_op(1, 0, 50));
        pool.submit(dummy_op(2, 0, 300));
        pool.submit(dummy_op(3, 0, 100));

        let ops = pool.drain(3);
        assert_eq!(ops[0].max_fee, 300);
        assert_eq!(ops[1].max_fee, 100);
        assert_eq!(ops[2].max_fee, 50);
    }

    #[test]
    fn drain_clears_dedup() {
        let pool = Mempool::new(100);
        pool.submit(dummy_op(1, 0, 100));
        pool.drain(1);
        // After drain, same sender+nonce can be resubmitted.
        assert!(pool.submit(dummy_op(1, 0, 100)));
    }
}
