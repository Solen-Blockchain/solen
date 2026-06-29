//! Post-quantum signatures — ML-DSA-65 (FIPS 204).
//!
//! An opt-in, quantum-resistant alternative to Ed25519 for high-assurance smart
//! accounts (`AuthMethod::MlDsa`). Shor's algorithm lets a quantum computer
//! recover an Ed25519/ECDSA private key from its public key; ML-DSA is a
//! module-lattice scheme with no known quantum break. ML-DSA-65 is NIST security
//! category 3.
//!
//! Verification is a deterministic pure function, so it is safe to run inside
//! consensus (every node reaches the same verdict). Signing is "hedged"
//! (randomized) and happens client-side only — it never runs on a validator —
//! so the signing randomness does not affect consensus. Ed25519 remains the
//! default everywhere; this is purely additive.

use fips204::ml_dsa_65;
use fips204::traits::{KeyGen, SerDes, Signer, Verifier};

use crate::SigningError;

/// ML-DSA-65 public-key length in bytes (1952).
pub const ML_DSA_PK_LEN: usize = ml_dsa_65::PK_LEN;
/// ML-DSA-65 signature length in bytes (3309).
pub const ML_DSA_SIG_LEN: usize = ml_dsa_65::SIG_LEN;

/// FIPS 204 context string. Left empty: the message signed here is the
/// operation's signing digest, which the caller already binds to the chain id /
/// domain, and an empty context keeps interoperability with standard ML-DSA
/// tooling. (Must match between sign and verify — both use this constant.)
const CTX: &[u8] = b"";

/// An ML-DSA-65 keypair (post-quantum). Used for client-side signing only.
pub struct MlDsaKeypair {
    sk: ml_dsa_65::PrivateKey,
    pk: ml_dsa_65::PublicKey,
}

impl MlDsaKeypair {
    /// Generate a new random keypair using the OS CSPRNG.
    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let (pk, sk) = ml_dsa_65::try_keygen_with_rng(&mut rng).expect("ml-dsa-65 keygen failed");
        Self { sk, pk }
    }

    /// Deterministically derive a keypair from a 32-byte seed (the FIPS 204 ξ),
    /// so a wallet can persist a single 32-byte secret exactly like Ed25519.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let (pk, sk) = ml_dsa_65::KG::keygen_from_seed(seed);
        Self { sk, pk }
    }

    /// The serialized public key (`ML_DSA_PK_LEN` bytes) — stored in
    /// `AuthMethod::MlDsa { public_key }`.
    pub fn public_key(&self) -> Vec<u8> {
        self.pk.clone().into_bytes().to_vec()
    }

    /// Sign a message, returning an `ML_DSA_SIG_LEN`-byte signature.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.sk
            .try_sign(message, CTX)
            .expect("ml-dsa-65 sign failed")
            .to_vec()
    }
}

/// Verify an ML-DSA-65 signature against a public key and message.
///
/// Deterministic — safe to run in consensus. Wrong-length keys or signatures
/// are rejected up front (never panics on attacker input).
pub fn verify_ml_dsa(
    public_key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), SigningError> {
    let pk_arr: [u8; ML_DSA_PK_LEN] = public_key
        .try_into()
        .map_err(|_| SigningError::InvalidPublicKey)?;
    let sig_arr: [u8; ML_DSA_SIG_LEN] = signature
        .try_into()
        .map_err(|_| SigningError::InvalidSignature)?;
    let pk = ml_dsa_65::PublicKey::try_from_bytes(pk_arr)
        .map_err(|_| SigningError::InvalidPublicKey)?;
    if pk.verify(message, &sig_arr, CTX) {
        Ok(())
    } else {
        Err(SigningError::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let kp = MlDsaKeypair::generate();
        let pk = kp.public_key();
        assert_eq!(pk.len(), ML_DSA_PK_LEN);
        let msg = b"hello quantum solen";
        let sig = kp.sign(msg);
        assert_eq!(sig.len(), ML_DSA_SIG_LEN);
        assert!(verify_ml_dsa(&pk, msg, &sig).is_ok());
    }

    #[test]
    fn wrong_message_fails() {
        let kp = MlDsaKeypair::generate();
        let sig = kp.sign(b"correct");
        assert!(verify_ml_dsa(&kp.public_key(), b"wrong", &sig).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let kp1 = MlDsaKeypair::generate();
        let kp2 = MlDsaKeypair::generate();
        let sig = kp1.sign(b"msg");
        assert!(verify_ml_dsa(&kp2.public_key(), b"msg", &sig).is_err());
    }

    #[test]
    fn from_seed_is_deterministic() {
        let seed = [7u8; 32];
        let a = MlDsaKeypair::from_seed(&seed);
        let b = MlDsaKeypair::from_seed(&seed);
        assert_eq!(a.public_key(), b.public_key());
        // A signature from one verifies under the other's (same) key.
        let sig = a.sign(b"x");
        assert!(verify_ml_dsa(&b.public_key(), b"x", &sig).is_ok());
    }

    #[test]
    fn malformed_inputs_rejected_not_panic() {
        let kp = MlDsaKeypair::generate();
        let sig = kp.sign(b"m");
        assert!(verify_ml_dsa(b"tooshort", b"m", &sig).is_err());
        assert!(verify_ml_dsa(&kp.public_key(), b"m", b"shortsig").is_err());
    }
}
