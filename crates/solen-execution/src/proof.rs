//! Proof verification for rollup batch commitments.

use solen_types::rollup::BatchCommitment;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProofError {
    #[error("invalid proof")]
    InvalidProof,
    #[error("unknown proof type")]
    UnknownProofType,
}

/// Verifies proofs submitted by rollup domains.
pub struct ProofVerifier;

impl ProofVerifier {
    pub fn verify(&self, _commitment: &BatchCommitment) -> Result<bool, ProofError> {
        todo!("implement proof verification")
    }
}
