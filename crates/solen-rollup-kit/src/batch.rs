//! Batch publisher: compresses and submits transaction batches to L1.
//!
//! The publisher takes batches from the sequencer, computes state commitments,
//! and publishes them as L1 operations.

use solen_crypto::blake3_hash;
use solen_types::rollup::BatchCommitment;
use solen_types::{Hash, RollupId};
use thiserror::Error;
use tracing::info;

use crate::sequencer::TransactionBatch;

#[derive(Debug, Error)]
pub enum PublishError {
    #[error("serialization failed: {0}")]
    Serialization(String),
    #[error("submission failed: {0}")]
    Submission(String),
}

/// Compresses a batch and produces an L1 commitment.
pub struct BatchPublisher {
    rollup_id: RollupId,
}

impl BatchPublisher {
    pub fn new(rollup_id: RollupId) -> Self {
        Self { rollup_id }
    }

    /// Convert a transaction batch into an L1 batch commitment.
    pub fn prepare_commitment(
        &self,
        batch: &TransactionBatch,
        _pre_state_root: Hash,
        post_state_root: Hash,
        proof: Vec<u8>,
    ) -> Result<BatchCommitment, PublishError> {
        let batch_data = serde_json::to_vec(&batch.transactions)
            .map_err(|e| PublishError::Serialization(e.to_string()))?;

        let data_hash = blake3_hash(&batch_data);

        info!(
            rollup_id = self.rollup_id,
            batch_index = batch.batch_index,
            tx_count = batch.transactions.len(),
            data_bytes = batch_data.len(),
            "batch commitment prepared"
        );

        Ok(BatchCommitment {
            rollup_id: self.rollup_id,
            batch_index: batch.batch_index,
            state_root: post_state_root,
            data_hash,
            proof,
        })
    }

    /// Compute the compressed data for a batch (for data availability).
    pub fn compress_batch(batch: &TransactionBatch) -> Result<Vec<u8>, PublishError> {
        serde_json::to_vec(&batch.transactions)
            .map_err(|e| PublishError::Serialization(e.to_string()))
    }
}

use serde_json;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sequencer::{L2Transaction, TransactionBatch};

    fn test_batch() -> TransactionBatch {
        TransactionBatch {
            rollup_id: 1,
            batch_index: 1,
            transactions: vec![
                L2Transaction {
                    sender: [1u8; 32],
                    nonce: 0,
                    data: vec![1, 2, 3],
                    gas_limit: 100,
                },
                L2Transaction {
                    sender: [2u8; 32],
                    nonce: 0,
                    data: vec![4, 5, 6],
                    gas_limit: 200,
                },
            ],
            timestamp_ms: 12345,
        }
    }

    #[test]
    fn prepare_commitment() {
        let publisher = BatchPublisher::new(1);
        let batch = test_batch();

        let commitment = publisher
            .prepare_commitment(&batch, [0u8; 32], [1u8; 32], vec![0xDE, 0xAD])
            .unwrap();

        assert_eq!(commitment.rollup_id, 1);
        assert_eq!(commitment.batch_index, 1);
        assert_eq!(commitment.state_root, [1u8; 32]);
        assert_ne!(commitment.data_hash, [0u8; 32]);
    }

    #[test]
    fn compress_batch_deterministic() {
        let batch = test_batch();
        let data1 = BatchPublisher::compress_batch(&batch).unwrap();
        let data2 = BatchPublisher::compress_batch(&batch).unwrap();
        assert_eq!(data1, data2);
    }
}
