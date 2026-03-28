//! Sequencer: orders transactions for a rollup domain.
//!
//! The sequencer collects L2 transactions, orders them, and produces
//! batches for submission to L1.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use solen_types::RollupId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SequencerError {
    #[error("sequencer is full")]
    Full,
    #[error("sequencer is stopped")]
    Stopped,
}

/// An L2 transaction submitted to the sequencer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2Transaction {
    pub sender: [u8; 32],
    pub nonce: u64,
    pub data: Vec<u8>,
    pub gas_limit: u64,
}

/// Configuration for the sequencer.
#[derive(Debug, Clone)]
pub struct SequencerConfig {
    pub rollup_id: RollupId,
    pub max_pending: usize,
    pub max_batch_size: usize,
    pub batch_interval_ms: u64,
}

impl Default for SequencerConfig {
    fn default() -> Self {
        Self {
            rollup_id: 1,
            max_pending: 10_000,
            max_batch_size: 100,
            batch_interval_ms: 1000,
        }
    }
}

/// An ordered batch of L2 transactions ready for submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionBatch {
    pub rollup_id: RollupId,
    pub batch_index: u64,
    pub transactions: Vec<L2Transaction>,
    pub timestamp_ms: u64,
}

/// The sequencer collects and orders L2 transactions.
pub struct Sequencer {
    config: SequencerConfig,
    pending: Arc<Mutex<VecDeque<L2Transaction>>>,
    batch_counter: Arc<Mutex<u64>>,
}

impl Sequencer {
    pub fn new(config: SequencerConfig) -> Self {
        Self {
            config,
            pending: Arc::new(Mutex::new(VecDeque::new())),
            batch_counter: Arc::new(Mutex::new(0)),
        }
    }

    /// Submit an L2 transaction to the sequencer.
    pub fn submit(&self, tx: L2Transaction) -> Result<(), SequencerError> {
        let mut pending = self.pending.lock().unwrap();
        if pending.len() >= self.config.max_pending {
            return Err(SequencerError::Full);
        }
        pending.push_back(tx);
        Ok(())
    }

    /// Drain pending transactions into a batch.
    pub fn produce_batch(&self) -> Option<TransactionBatch> {
        let mut pending = self.pending.lock().unwrap();
        if pending.is_empty() {
            return None;
        }

        let n = self.config.max_batch_size.min(pending.len());
        let transactions: Vec<L2Transaction> = pending.drain(..n).collect();

        let mut counter = self.batch_counter.lock().unwrap();
        *counter += 1;
        let batch_index = *counter;

        Some(TransactionBatch {
            rollup_id: self.config.rollup_id,
            batch_index,
            transactions,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        })
    }

    /// Number of pending transactions.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    pub fn rollup_id(&self) -> RollupId {
        self.config.rollup_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_tx(nonce: u64) -> L2Transaction {
        L2Transaction {
            sender: [1u8; 32],
            nonce,
            data: vec![0; 10],
            gas_limit: 100,
        }
    }

    #[test]
    fn submit_and_batch() {
        let seq = Sequencer::new(SequencerConfig::default());
        seq.submit(dummy_tx(0)).unwrap();
        seq.submit(dummy_tx(1)).unwrap();
        seq.submit(dummy_tx(2)).unwrap();

        let batch = seq.produce_batch().unwrap();
        assert_eq!(batch.batch_index, 1);
        assert_eq!(batch.transactions.len(), 3);
        assert_eq!(seq.pending_count(), 0);
    }

    #[test]
    fn respects_max_batch_size() {
        let config = SequencerConfig {
            max_batch_size: 2,
            ..Default::default()
        };
        let seq = Sequencer::new(config);
        for i in 0..5 {
            seq.submit(dummy_tx(i)).unwrap();
        }

        let batch1 = seq.produce_batch().unwrap();
        assert_eq!(batch1.transactions.len(), 2);
        assert_eq!(seq.pending_count(), 3);

        let batch2 = seq.produce_batch().unwrap();
        assert_eq!(batch2.batch_index, 2);
        assert_eq!(batch2.transactions.len(), 2);
    }

    #[test]
    fn empty_batch_returns_none() {
        let seq = Sequencer::new(SequencerConfig::default());
        assert!(seq.produce_batch().is_none());
    }

    #[test]
    fn respects_max_pending() {
        let config = SequencerConfig {
            max_pending: 2,
            ..Default::default()
        };
        let seq = Sequencer::new(config);
        seq.submit(dummy_tx(0)).unwrap();
        seq.submit(dummy_tx(1)).unwrap();
        assert!(matches!(seq.submit(dummy_tx(2)), Err(SequencerError::Full)));
    }
}
