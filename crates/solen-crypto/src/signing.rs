//! Ed25519 signature creation and verification.

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SigningError {
    #[error("invalid signature")]
    InvalidSignature,
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("unsupported scheme")]
    UnsupportedScheme,
}

/// A keypair for Ed25519 signing.
pub struct Keypair {
    signing_key: SigningKey,
}

impl Keypair {
    /// Generate a new random keypair.
    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();
        Self {
            signing_key: SigningKey::generate(&mut rng),
        }
    }

    /// Create a keypair from a 32-byte secret seed.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(seed),
        }
    }

    /// Returns the 32-byte public key.
    pub fn public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// Sign a message, returning a 64-byte signature.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing_key.sign(message).to_bytes()
    }
}

/// Verify an Ed25519 signature against a public key and message.
///
/// Uses `verify_strict` (not `verify`) so non-canonical signatures and
/// small-order public keys are rejected — signatures are non-malleable and
/// agree across implementations. Every standard Ed25519 signer (including this
/// module's `sign`) already produces canonical signatures, so this only
/// rejects deliberately-crafted malleable ones. NOTE: consensus-affecting —
/// ship via a coordinated upgrade so all nodes verify identically.
pub fn verify(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> Result<(), SigningError> {
    let verifying_key =
        VerifyingKey::from_bytes(public_key).map_err(|_| SigningError::InvalidPublicKey)?;
    let sig =
        ed25519_dalek::Signature::from_bytes(signature);
    verifying_key
        .verify_strict(message, &sig)
        .map_err(|_| SigningError::InvalidSignature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify() {
        let kp = Keypair::generate();
        let msg = b"hello solen";
        let sig = kp.sign(msg);
        assert!(verify(&kp.public_key(), msg, &sig).is_ok());
    }

    #[test]
    fn wrong_message_fails() {
        let kp = Keypair::generate();
        let sig = kp.sign(b"correct");
        assert!(verify(&kp.public_key(), b"wrong", &sig).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let sig = kp1.sign(b"msg");
        assert!(verify(&kp2.public_key(), b"msg", &sig).is_err());
    }

    #[test]
    fn deterministic_from_seed() {
        let seed = [42u8; 32];
        let kp1 = Keypair::from_seed(&seed);
        let kp2 = Keypair::from_seed(&seed);
        assert_eq!(kp1.public_key(), kp2.public_key());

        let sig1 = kp1.sign(b"test");
        let sig2 = kp2.sign(b"test");
        assert_eq!(sig1, sig2);
    }
}
