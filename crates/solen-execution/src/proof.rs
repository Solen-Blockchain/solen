//! Proof verification for rollup batch commitments.
//!
//! The L1 settlement layer verifies proofs submitted by rollup domains.
//! Each rollup registers a proof type; the verifier registry dispatches
//! to the appropriate backend.

use std::collections::HashMap;
use std::sync::Arc;

use solen_types::rollup::BatchCommitment;
use solen_types::RollupId;
use thiserror::Error;
use tracing::info;

#[derive(Debug, Error)]
pub enum ProofError {
    #[error("invalid proof")]
    InvalidProof,
    #[error("unknown proof type for rollup {0}")]
    UnknownProofType(RollupId),
    #[error("rollup not registered: {0}")]
    RollupNotRegistered(RollupId),
    #[error("verifier error: {0}")]
    VerifierError(String),
}

/// Trait for L1-side proof verifiers.
pub trait ProofVerifierBackend: Send + Sync {
    fn verify(
        &self,
        pre_state_root: &[u8; 32],
        post_state_root: &[u8; 32],
        data_hash: &[u8; 32],
        proof: &[u8],
    ) -> Result<bool, String>;

    fn proof_type(&self) -> &str;
}

/// A mock verifier matching the MockProver from solen-rollup-kit.
pub struct MockVerifier;

impl ProofVerifierBackend for MockVerifier {
    fn verify(
        &self,
        pre_state_root: &[u8; 32],
        post_state_root: &[u8; 32],
        data_hash: &[u8; 32],
        proof: &[u8],
    ) -> Result<bool, String> {
        if proof.len() != 32 {
            return Ok(false);
        }
        let mut preimage = Vec::new();
        preimage.extend_from_slice(b"mock_proof:");
        preimage.extend_from_slice(pre_state_root);
        preimage.extend_from_slice(post_state_root);
        preimage.extend_from_slice(data_hash);
        let expected = solen_crypto::blake3_hash(&preimage);
        Ok(proof == expected)
    }

    fn proof_type(&self) -> &str {
        "mock"
    }
}

/// Registry of rollup proof verifiers.
pub struct ProofVerifierRegistry {
    /// Rollup ID -> (pre_state_root, verifier backend)
    rollups: HashMap<RollupId, RollupVerifierState>,
    verifiers: HashMap<String, Arc<dyn ProofVerifierBackend>>,
    /// All verified batches, ordered by submission time.
    verified_batches: Vec<VerifiedBatch>,
}

struct RollupVerifierState {
    proof_type: String,
    last_verified_state_root: [u8; 32],
}

/// A verified batch record.
#[derive(Debug, Clone)]
pub struct VerifiedBatch {
    pub rollup_id: RollupId,
    pub batch_index: u64,
    pub state_root: [u8; 32],
    pub data_hash: [u8; 32],
    pub pre_state_root: [u8; 32],
}

impl ProofVerifierRegistry {
    pub fn new() -> Self {
        Self {
            rollups: HashMap::new(),
            verifiers: HashMap::new(),
            verified_batches: Vec::new(),
        }
    }

    /// Register a proof verifier backend.
    pub fn register_verifier(&mut self, backend: Arc<dyn ProofVerifierBackend>) {
        self.verifiers
            .insert(backend.proof_type().to_string(), backend);
    }

    /// Register a rollup domain with its proof type and initial state root.
    pub fn register_rollup(
        &mut self,
        rollup_id: RollupId,
        proof_type: &str,
        genesis_state_root: [u8; 32],
    ) -> Result<(), ProofError> {
        if !self.verifiers.contains_key(proof_type) {
            return Err(ProofError::UnknownProofType(rollup_id));
        }
        self.rollups.insert(
            rollup_id,
            RollupVerifierState {
                proof_type: proof_type.to_string(),
                last_verified_state_root: genesis_state_root,
            },
        );
        info!(rollup_id, proof_type, "rollup registered for proof verification");
        Ok(())
    }

    /// Verify a batch commitment from a rollup.
    /// On success, updates the rollup's verified state root.
    pub fn verify_batch(&mut self, commitment: &BatchCommitment) -> Result<bool, ProofError> {
        let rollup = self
            .rollups
            .get(&commitment.rollup_id)
            .ok_or(ProofError::RollupNotRegistered(commitment.rollup_id))?;

        let verifier = self
            .verifiers
            .get(&rollup.proof_type)
            .ok_or(ProofError::UnknownProofType(commitment.rollup_id))?;

        let pre_state = rollup.last_verified_state_root;

        let valid = verifier
            .verify(
                &pre_state,
                &commitment.state_root,
                &commitment.data_hash,
                &commitment.proof,
            )
            .map_err(|e| ProofError::VerifierError(e))?;

        if valid {
            // Record the verified batch.
            self.verified_batches.push(VerifiedBatch {
                rollup_id: commitment.rollup_id,
                batch_index: commitment.batch_index,
                state_root: commitment.state_root,
                data_hash: commitment.data_hash,
                pre_state_root: pre_state,
            });

            // Update the verified state root.
            if let Some(r) = self.rollups.get_mut(&commitment.rollup_id) {
                r.last_verified_state_root = commitment.state_root;
            }
            info!(
                rollup_id = commitment.rollup_id,
                batch_index = commitment.batch_index,
                "batch proof verified"
            );
        }

        Ok(valid)
    }

    /// Get the last verified state root for a rollup.
    pub fn last_state_root(&self, rollup_id: RollupId) -> Option<[u8; 32]> {
        self.rollups.get(&rollup_id).map(|r| r.last_verified_state_root)
    }

    /// Get verified batches for a rollup, newest first.
    pub fn get_verified_batches(&self, rollup_id: RollupId, limit: usize) -> Vec<&VerifiedBatch> {
        self.verified_batches
            .iter()
            .rev()
            .filter(|b| b.rollup_id == rollup_id)
            .take(limit)
            .collect()
    }

    /// Get total number of verified batches for a rollup.
    pub fn batch_count(&self, rollup_id: RollupId) -> usize {
        self.verified_batches.iter().filter(|b| b.rollup_id == rollup_id).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solen_types::rollup::BatchCommitment;

    #[test]
    fn register_and_verify() {
        let mut registry = ProofVerifierRegistry::new();
        registry.register_verifier(Arc::new(MockVerifier));
        registry
            .register_rollup(1, "mock", [0u8; 32])
            .unwrap();

        // Generate a valid mock proof.
        let pre = [0u8; 32];
        let post = [1u8; 32];
        let data_hash = [2u8; 32];

        let mut preimage = Vec::new();
        preimage.extend_from_slice(b"mock_proof:");
        preimage.extend_from_slice(&pre);
        preimage.extend_from_slice(&post);
        preimage.extend_from_slice(&data_hash);
        let proof = solen_crypto::blake3_hash(&preimage).to_vec();

        let commitment = BatchCommitment {
            rollup_id: 1,
            batch_index: 1,
            state_root: post,
            data_hash,
            proof,
        };

        assert!(registry.verify_batch(&commitment).unwrap());
        assert_eq!(registry.last_state_root(1), Some(post));
    }

    #[test]
    fn invalid_proof_rejected() {
        let mut registry = ProofVerifierRegistry::new();
        registry.register_verifier(Arc::new(MockVerifier));
        registry
            .register_rollup(1, "mock", [0u8; 32])
            .unwrap();

        let commitment = BatchCommitment {
            rollup_id: 1,
            batch_index: 1,
            state_root: [1u8; 32],
            data_hash: [2u8; 32],
            proof: vec![0u8; 32], // bad proof
        };

        assert!(!registry.verify_batch(&commitment).unwrap());
    }

    #[test]
    fn unregistered_rollup_rejected() {
        let mut registry = ProofVerifierRegistry::new();
        registry.register_verifier(Arc::new(MockVerifier));

        let commitment = BatchCommitment {
            rollup_id: 99,
            batch_index: 1,
            state_root: [1u8; 32],
            data_hash: [2u8; 32],
            proof: vec![],
        };

        assert!(matches!(
            registry.verify_batch(&commitment),
            Err(ProofError::RollupNotRegistered(99))
        ));
    }

    #[test]
    fn state_root_chain() {
        let mut registry = ProofVerifierRegistry::new();
        registry.register_verifier(Arc::new(MockVerifier));
        registry
            .register_rollup(1, "mock", [0u8; 32])
            .unwrap();

        // Batch 1: state 0 -> 1
        let proof1 = make_mock_proof(&[0u8; 32], &[1u8; 32], &[10u8; 32]);
        let c1 = BatchCommitment {
            rollup_id: 1,
            batch_index: 1,
            state_root: [1u8; 32],
            data_hash: [10u8; 32],
            proof: proof1,
        };
        assert!(registry.verify_batch(&c1).unwrap());

        // Batch 2: state 1 -> 2 (pre_state is now 1, not 0)
        let proof2 = make_mock_proof(&[1u8; 32], &[2u8; 32], &[20u8; 32]);
        let c2 = BatchCommitment {
            rollup_id: 1,
            batch_index: 2,
            state_root: [2u8; 32],
            data_hash: [20u8; 32],
            proof: proof2,
        };
        assert!(registry.verify_batch(&c2).unwrap());
        assert_eq!(registry.last_state_root(1), Some([2u8; 32]));
    }

    fn make_mock_proof(pre: &[u8; 32], post: &[u8; 32], data_hash: &[u8; 32]) -> Vec<u8> {
        let mut preimage = Vec::new();
        preimage.extend_from_slice(b"mock_proof:");
        preimage.extend_from_slice(pre);
        preimage.extend_from_slice(post);
        preimage.extend_from_slice(data_hash);
        solen_crypto::blake3_hash(&preimage).to_vec()
    }
}
