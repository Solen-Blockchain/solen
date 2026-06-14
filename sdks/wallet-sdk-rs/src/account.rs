//! Smart account creation, configuration, and management.

use solen_crypto::Keypair;
use solen_types::account::{Account, AuthMethod};
use solen_types::AccountId;

/// Builder for constructing smart account configurations.
pub struct SmartAccountBuilder {
    auth_methods: Vec<AuthMethod>,
}

impl SmartAccountBuilder {
    pub fn new() -> Self {
        Self {
            auth_methods: Vec::new(),
        }
    }

    /// Add an Ed25519 owner key.
    pub fn with_ed25519_owner(mut self, public_key: [u8; 32]) -> Self {
        self.auth_methods.push(AuthMethod::Ed25519 { public_key });
        self
    }

    /// Add an owner from a keypair.
    pub fn with_keypair(self, kp: &Keypair) -> Self {
        self.with_ed25519_owner(kp.public_key())
    }

    /// Add a passkey auth method (P-256/secp256r1) bound to a WebAuthn
    /// Relying Party ID and an allowlist of origins. Pass an empty `rp_id`
    /// or empty `origins` to leave that binding unenforced.
    pub fn with_passkey(
        mut self,
        credential_id: Vec<u8>,
        public_key_x: [u8; 32],
        public_key_y: [u8; 32],
        rp_id: String,
        origins: Vec<String>,
    ) -> Self {
        self.auth_methods.push(AuthMethod::Passkey {
            credential_id,
            public_key_x,
            public_key_y,
            rp_id,
            origins,
        });
        self
    }

    /// Add a guardian for recovery.
    pub fn with_guardian(mut self, guardian_id: AccountId) -> Self {
        self.auth_methods
            .push(AuthMethod::Guardian { guardian_id });
        self
    }

    /// Add threshold (multi-sig) authentication.
    pub fn with_threshold(mut self, signers: Vec<[u8; 32]>, threshold: u16) -> Self {
        self.auth_methods.push(AuthMethod::Threshold {
            signers,
            threshold,
        });
        self
    }

    /// Build the account configuration (without deploying).
    pub fn build(self, id: AccountId) -> Account {
        Account {
            id,
            code_hash: [0u8; 32],
            auth_methods: self.auth_methods,
            nonce: 0,
            balance: 0,
        }
    }

    /// Get the configured auth methods.
    pub fn auth_methods(&self) -> &[AuthMethod] {
        &self.auth_methods
    }
}

impl Default for SmartAccountBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_account_with_ed25519() {
        let kp = Keypair::generate();
        let id = [1u8; 32];

        let account = SmartAccountBuilder::new()
            .with_keypair(&kp)
            .build(id);

        assert_eq!(account.id, id);
        assert_eq!(account.auth_methods.len(), 1);
        assert_eq!(account.nonce, 0);
        assert_eq!(account.balance, 0);
    }

    #[test]
    fn build_multisig_account() {
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();

        let account = SmartAccountBuilder::new()
            .with_threshold(vec![kp1.public_key(), kp2.public_key()], 2)
            .with_guardian([99u8; 32])
            .build([1u8; 32]);

        assert_eq!(account.auth_methods.len(), 2);
    }
}
