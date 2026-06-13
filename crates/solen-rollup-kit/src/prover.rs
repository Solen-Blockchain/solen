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

/// Domain-separation tag for committee batch attestations. Versioned so the
/// signed-message format can evolve without ambiguity.
pub const ATTESTATION_DOMAIN: &[u8] = b"solen-rollup-attestation:v1";

/// The exact message a committee attestor signs for a batch. Binds the rollup,
/// the batch index, and the full pre→post state transition over the batch data
/// so a signature can't be replayed onto a different rollup, batch, or root.
pub fn committee_attestation_message(
    rollup_id: u64,
    batch_index: u64,
    pre_state_root: &Hash,
    post_state_root: &Hash,
    data_hash: &Hash,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(ATTESTATION_DOMAIN.len() + 16 + 96);
    msg.extend_from_slice(ATTESTATION_DOMAIN);
    msg.extend_from_slice(&rollup_id.to_le_bytes());
    msg.extend_from_slice(&batch_index.to_le_bytes());
    msg.extend_from_slice(pre_state_root);
    msg.extend_from_slice(post_state_root);
    msg.extend_from_slice(data_hash);
    msg
}

/// Verify a validity-committee proof for a batch.
///
/// Trust model: instead of trusting a single sequencer, the L1 accepts a batch
/// only if a `threshold` of distinct registered attestors signed off on the
/// state transition. This is a real, on-chain-verifiable proof (the standard
/// "validity committee" / DAC model that rollups use before ZK validity
/// proofs), as opposed to the insecure `MockProver`.
///
/// `proof` layout: count[4, LE] then `count` entries of
/// `attestor_index[4, LE] ‖ signature[64]`. Entries referencing an
/// out-of-range attestor, duplicating an already-counted attestor, or failing
/// verification are ignored; the batch is accepted iff at least `threshold`
/// DISTINCT attestors produced a valid signature.
#[allow(clippy::too_many_arguments)]
pub fn verify_committee_attestation(
    rollup_id: u64,
    batch_index: u64,
    pre_state_root: &Hash,
    post_state_root: &Hash,
    data_hash: &Hash,
    attestors: &[[u8; 32]],
    threshold: usize,
    proof: &[u8],
) -> Result<bool, ProverError> {
    if threshold == 0 || attestors.is_empty() || threshold > attestors.len() {
        return Err(ProverError::InvalidFormat);
    }
    if proof.len() < 4 {
        return Err(ProverError::InvalidFormat);
    }
    let count = u32::from_le_bytes([proof[0], proof[1], proof[2], proof[3]]) as usize;

    let msg = committee_attestation_message(
        rollup_id, batch_index, pre_state_root, post_state_root, data_hash,
    );

    let mut valid_signers = std::collections::BTreeSet::new();
    let mut off = 4usize;
    for _ in 0..count {
        if off + 68 > proof.len() {
            return Err(ProverError::InvalidFormat);
        }
        let idx = u32::from_le_bytes([proof[off], proof[off + 1], proof[off + 2], proof[off + 3]]) as usize;
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&proof[off + 4..off + 68]);
        off += 68;

        if idx >= attestors.len() || valid_signers.contains(&idx) {
            continue;
        }
        if solen_crypto::verify(&attestors[idx], &msg, &sig).is_ok() {
            valid_signers.insert(idx);
        }
    }

    Ok(valid_signers.len() >= threshold)
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

    // ── Committee attestation ─────────────────────────────────────

    fn attestor(seed: u8) -> solen_crypto::Keypair {
        solen_crypto::Keypair::from_seed(&[seed; 32])
    }

    /// Build a committee proof from (attestor_index, keypair) signers over the
    /// canonical message for the given batch transition.
    fn committee_proof(
        rollup_id: u64,
        batch_index: u64,
        pre: &Hash,
        post: &Hash,
        data_hash: &Hash,
        signers: &[(u32, &solen_crypto::Keypair)],
    ) -> Vec<u8> {
        let msg = committee_attestation_message(rollup_id, batch_index, pre, post, data_hash);
        let mut proof = (signers.len() as u32).to_le_bytes().to_vec();
        for (idx, kp) in signers {
            proof.extend_from_slice(&idx.to_le_bytes());
            proof.extend_from_slice(&kp.sign(&msg));
        }
        proof
    }

    #[test]
    fn committee_accepts_threshold_signatures() {
        let a = [attestor(1), attestor(2), attestor(3)];
        let attestors: Vec<[u8; 32]> = a.iter().map(|k| k.public_key()).collect();
        let (pre, post, dh) = ([0u8; 32], [9u8; 32], [7u8; 32]);
        // 2-of-3 signed.
        let proof = committee_proof(1, 0, &pre, &post, &dh, &[(0, &a[0]), (1, &a[1])]);
        assert!(verify_committee_attestation(1, 0, &pre, &post, &dh, &attestors, 2, &proof).unwrap());
    }

    #[test]
    fn committee_rejects_below_threshold() {
        let a = [attestor(1), attestor(2), attestor(3)];
        let attestors: Vec<[u8; 32]> = a.iter().map(|k| k.public_key()).collect();
        let (pre, post, dh) = ([0u8; 32], [9u8; 32], [7u8; 32]);
        // Only 1 signed but threshold is 2.
        let proof = committee_proof(1, 0, &pre, &post, &dh, &[(0, &a[0])]);
        assert!(!verify_committee_attestation(1, 0, &pre, &post, &dh, &attestors, 2, &proof).unwrap());
    }

    #[test]
    fn committee_rejects_duplicate_signer() {
        let a = [attestor(1), attestor(2), attestor(3)];
        let attestors: Vec<[u8; 32]> = a.iter().map(|k| k.public_key()).collect();
        let (pre, post, dh) = ([0u8; 32], [9u8; 32], [7u8; 32]);
        // Same attestor counted twice — must NOT reach a threshold of 2.
        let proof = committee_proof(1, 0, &pre, &post, &dh, &[(0, &a[0]), (0, &a[0])]);
        assert!(!verify_committee_attestation(1, 0, &pre, &post, &dh, &attestors, 2, &proof).unwrap());
    }

    #[test]
    fn committee_rejects_wrong_transition() {
        let a = [attestor(1), attestor(2)];
        let attestors: Vec<[u8; 32]> = a.iter().map(|k| k.public_key()).collect();
        let (pre, post, dh) = ([0u8; 32], [9u8; 32], [7u8; 32]);
        // Signers attest a DIFFERENT post root; verifying against `post` fails.
        let bad_post = [8u8; 32];
        let proof = committee_proof(1, 0, &pre, &bad_post, &dh, &[(0, &a[0]), (1, &a[1])]);
        assert!(!verify_committee_attestation(1, 0, &pre, &post, &dh, &attestors, 2, &proof).unwrap());
    }

    #[test]
    fn committee_rejects_foreign_signature() {
        let a = [attestor(1), attestor(2)];
        let attestors: Vec<[u8; 32]> = a.iter().map(|k| k.public_key()).collect();
        let (pre, post, dh) = ([0u8; 32], [9u8; 32], [7u8; 32]);
        // A non-committee key signs but claims index 0 — must not count.
        let outsider = attestor(99);
        let proof = committee_proof(1, 0, &pre, &post, &dh, &[(0, &outsider), (1, &a[1])]);
        assert!(!verify_committee_attestation(1, 0, &pre, &post, &dh, &attestors, 2, &proof).unwrap());
    }
}
