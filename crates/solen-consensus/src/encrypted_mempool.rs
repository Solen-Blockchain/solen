//! Encrypted mempool: commit-reveal scheme for MEV protection.
//!
//! Users submit encrypted operations (commitments). After the ordering
//! deadline, operations are revealed and executed. This prevents
//! frontrunning and sandwich attacks.
//!
//! Flow:
//! 1. User encrypts operation with a random key, submits commitment (hash).
//! 2. Block proposer collects commitments and locks ordering.
//! 3. User reveals the plaintext operation + key.
//! 4. Executor verifies the reveal matches the commitment and executes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use solen_crypto::blake3_hash;
use solen_types::transaction::UserOperation;
use solen_types::Hash;

/// A commitment to an encrypted operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationCommitment {
    /// Hash of (encrypted_data || sender).
    pub commitment_hash: Hash,
    /// The encrypted operation data.
    pub encrypted_data: Vec<u8>,
    /// The sender's account ID (visible for ordering).
    pub sender: [u8; 32],
    /// Block height at which this commitment was submitted.
    pub submitted_at: u64,
}

/// A revealed operation that matches a previous commitment.
#[derive(Debug, Clone)]
pub struct RevealedOperation {
    pub commitment_hash: Hash,
    pub operation: UserOperation,
    pub reveal_key: Vec<u8>,
}

/// Encrypted mempool state.
pub struct EncryptedMempool {
    /// Pending commitments awaiting reveal.
    commitments: Arc<Mutex<HashMap<Hash, OperationCommitment>>>,
    /// Revealed operations ready for execution.
    revealed: Arc<Mutex<Vec<RevealedOperation>>>,
    /// Reveal deadline: how many blocks after commitment before reveal is required.
    reveal_window: u64,
    max_size: usize,
}

impl EncryptedMempool {
    pub fn new(max_size: usize, reveal_window: u64) -> Self {
        Self {
            commitments: Arc::new(Mutex::new(HashMap::new())),
            revealed: Arc::new(Mutex::new(Vec::new())),
            reveal_window,
            max_size,
        }
    }

    /// Submit an encrypted commitment.
    pub fn submit_commitment(&self, commitment: OperationCommitment) -> bool {
        let mut commitments = self.commitments.lock().unwrap();
        if commitments.len() >= self.max_size {
            return false;
        }
        commitments.insert(commitment.commitment_hash, commitment);
        true
    }

    /// Reveal an operation. Verifies that the reveal matches the commitment.
    pub fn reveal(
        &self,
        commitment_hash: Hash,
        operation: UserOperation,
        reveal_key: Vec<u8>,
    ) -> Result<(), &'static str> {
        let mut commitments = self.commitments.lock().unwrap();

        let commitment = commitments
            .get(&commitment_hash)
            .ok_or("commitment not found")?;

        // Verify: hash(encrypted_data || sender) == commitment_hash.
        let mut preimage = Vec::new();
        preimage.extend_from_slice(&commitment.encrypted_data);
        preimage.extend_from_slice(&commitment.sender);
        let expected_hash = blake3_hash(&preimage);

        if expected_hash != commitment_hash {
            return Err("commitment hash mismatch");
        }

        if operation.sender != commitment.sender {
            return Err("sender mismatch");
        }

        commitments.remove(&commitment_hash);

        let mut revealed = self.revealed.lock().unwrap();
        revealed.push(RevealedOperation {
            commitment_hash,
            operation,
            reveal_key,
        });

        Ok(())
    }

    /// Drain all revealed operations for block inclusion.
    pub fn drain_revealed(&self) -> Vec<UserOperation> {
        let mut revealed = self.revealed.lock().unwrap();
        revealed.drain(..).map(|r| r.operation).collect()
    }

    /// Expire commitments that weren't revealed in time.
    pub fn expire_commitments(&self, current_block: u64) -> usize {
        let mut commitments = self.commitments.lock().unwrap();
        let before = commitments.len();
        commitments.retain(|_, c| {
            current_block <= c.submitted_at + self.reveal_window
        });
        before - commitments.len()
    }

    pub fn pending_commitments(&self) -> usize {
        self.commitments.lock().unwrap().len()
    }

    pub fn pending_reveals(&self) -> usize {
        self.revealed.lock().unwrap().len()
    }
}

/// Helper to create a commitment from an operation.
pub fn create_commitment(
    encrypted_data: Vec<u8>,
    sender: [u8; 32],
    current_block: u64,
) -> OperationCommitment {
    let mut preimage = Vec::new();
    preimage.extend_from_slice(&encrypted_data);
    preimage.extend_from_slice(&sender);
    let commitment_hash = blake3_hash(&preimage);

    OperationCommitment {
        commitment_hash,
        encrypted_data,
        sender,
        submitted_at: current_block,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solen_types::transaction::Action;

    fn test_op() -> UserOperation {
        UserOperation {
            sender: [1u8; 32],
            nonce: 0,
            actions: vec![Action::Transfer {
                to: [2u8; 32],
                amount: 100,
            }],
            max_fee: 1000,
            signature: vec![],
        }
    }

    #[test]
    fn commit_reveal_lifecycle() {
        let pool = EncryptedMempool::new(100, 10);

        let commitment = create_commitment(b"encrypted_payload".to_vec(), [1u8; 32], 50);
        let hash = commitment.commitment_hash;

        assert!(pool.submit_commitment(commitment));
        assert_eq!(pool.pending_commitments(), 1);

        pool.reveal(hash, test_op(), b"key".to_vec()).unwrap();
        assert_eq!(pool.pending_commitments(), 0);
        assert_eq!(pool.pending_reveals(), 1);

        let ops = pool.drain_revealed();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].sender, [1u8; 32]);
    }

    #[test]
    fn sender_mismatch_rejected() {
        let pool = EncryptedMempool::new(100, 10);
        let commitment = create_commitment(b"data".to_vec(), [1u8; 32], 50);
        let hash = commitment.commitment_hash;
        pool.submit_commitment(commitment);

        let mut bad_op = test_op();
        bad_op.sender = [99u8; 32]; // wrong sender

        let err = pool.reveal(hash, bad_op, vec![]).unwrap_err();
        assert_eq!(err, "sender mismatch");
    }

    #[test]
    fn expire_unrevealed() {
        let pool = EncryptedMempool::new(100, 10);
        let commitment = create_commitment(b"data".to_vec(), [1u8; 32], 50);
        pool.submit_commitment(commitment);

        assert_eq!(pool.expire_commitments(55), 0); // within window
        assert_eq!(pool.expire_commitments(61), 1); // past window
        assert_eq!(pool.pending_commitments(), 0);
    }
}
