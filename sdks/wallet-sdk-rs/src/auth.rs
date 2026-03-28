//! Authentication: Ed25519 key management and signature verification.

use solen_crypto::{verify, Keypair, SigningError};
use solen_types::account::AuthMethod;

/// Verify that a signature matches one of the account's auth methods.
pub fn verify_against_auth_methods(
    auth_methods: &[AuthMethod],
    message: &[u8],
    signature: &[u8],
) -> bool {
    if signature.len() != 64 {
        return false;
    }
    let mut sig = [0u8; 64];
    sig.copy_from_slice(signature);

    auth_methods.iter().any(|method| match method {
        AuthMethod::Ed25519 { public_key } => verify(public_key, message, &sig).is_ok(),
        _ => false,
    })
}

/// Create an Ed25519 auth method from a keypair.
pub fn ed25519_auth(kp: &Keypair) -> AuthMethod {
    AuthMethod::Ed25519 {
        public_key: kp.public_key(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_valid_signature() {
        let kp = Keypair::generate();
        let auth = vec![ed25519_auth(&kp)];
        let msg = b"test message";
        let sig = kp.sign(msg);

        assert!(verify_against_auth_methods(&auth, msg, &sig));
    }

    #[test]
    fn reject_invalid_signature() {
        let kp = Keypair::generate();
        let auth = vec![ed25519_auth(&kp)];

        assert!(!verify_against_auth_methods(&auth, b"msg", &[0u8; 64]));
    }

    #[test]
    fn reject_wrong_length() {
        let kp = Keypair::generate();
        let auth = vec![ed25519_auth(&kp)];

        assert!(!verify_against_auth_methods(&auth, b"msg", &[0u8; 32]));
    }
}
