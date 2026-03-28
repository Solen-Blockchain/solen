//! Prover adapter: interface for pluggable proof systems.
//!
//! Rollup domains choose between validity proofs (ZK) and fraud proofs.
//! This module provides the trait and a simple mock implementation.

use solen_types::Hash;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProverError {
    #[error("proof generation failed: {0}")]
    GenerationFailed(String),
    #[error("proof verification failed")]
    VerificationFailed,
    #[error("invalid proof format")]
    InvalidFormat,
}

/// Trait for proof system backends.
pub trait ProverBackend: Send + Sync {
    /// Generate a proof for a state transition.
    fn generate_proof(
        &self,
        pre_state_root: &Hash,
        post_state_root: &Hash,
        batch_data: &[u8],
    ) -> Result<Vec<u8>, ProverError>;

    /// Verify a proof.
    fn verify_proof(
        &self,
        pre_state_root: &Hash,
        post_state_root: &Hash,
        batch_data_hash: &Hash,
        proof: &[u8],
    ) -> Result<bool, ProverError>;

    /// The proof system type identifier.
    fn proof_type(&self) -> &str;
}

/// A mock prover that produces simple hash-based proofs.
/// For development and testing only — not cryptographically secure.
pub struct MockProver;

impl ProverBackend for MockProver {
    fn generate_proof(
        &self,
        pre_state_root: &Hash,
        post_state_root: &Hash,
        batch_data: &[u8],
    ) -> Result<Vec<u8>, ProverError> {
        let mut preimage = Vec::new();
        preimage.extend_from_slice(b"mock_proof:");
        preimage.extend_from_slice(pre_state_root);
        preimage.extend_from_slice(post_state_root);
        preimage.extend_from_slice(&solen_crypto::blake3_hash(batch_data));
        Ok(solen_crypto::blake3_hash(&preimage).to_vec())
    }

    fn verify_proof(
        &self,
        pre_state_root: &Hash,
        post_state_root: &Hash,
        batch_data_hash: &Hash,
        proof: &[u8],
    ) -> Result<bool, ProverError> {
        if proof.len() != 32 {
            return Err(ProverError::InvalidFormat);
        }

        let mut preimage = Vec::new();
        preimage.extend_from_slice(b"mock_proof:");
        preimage.extend_from_slice(pre_state_root);
        preimage.extend_from_slice(post_state_root);
        preimage.extend_from_slice(batch_data_hash);

        let expected = solen_crypto::blake3_hash(&preimage);
        Ok(proof == expected)
    }

    fn proof_type(&self) -> &str {
        "mock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_prover_roundtrip() {
        let prover = MockProver;
        let pre = [0u8; 32];
        let post = [1u8; 32];
        let batch_data = b"some batch data";

        let proof = prover.generate_proof(&pre, &post, batch_data).unwrap();
        assert_eq!(proof.len(), 32);

        let data_hash = solen_crypto::blake3_hash(batch_data);
        let valid = prover.verify_proof(&pre, &post, &data_hash, &proof).unwrap();
        assert!(valid);
    }

    #[test]
    fn mock_prover_rejects_bad_proof() {
        let prover = MockProver;
        let pre = [0u8; 32];
        let post = [1u8; 32];
        let data_hash = [2u8; 32];

        let bad_proof = [0u8; 32];
        let valid = prover
            .verify_proof(&pre, &post, &data_hash, &bad_proof)
            .unwrap();
        assert!(!valid);
    }

    #[test]
    fn mock_prover_rejects_wrong_length() {
        let prover = MockProver;
        let result = prover.verify_proof(&[0; 32], &[0; 32], &[0; 32], &[0; 16]);
        assert!(matches!(result, Err(ProverError::InvalidFormat)));
    }
}
