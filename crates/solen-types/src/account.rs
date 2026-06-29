//! Smart account types.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

use crate::{AccountId, Hash};

/// Authentication method for a smart account.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum AuthMethod {
    /// WebAuthn/Passkey: P-256 (secp256r1) ECDSA signature verification.
    /// The signature field carries: authenticatorData || clientDataJSON || r[32] || s[32].
    /// The challenge in clientDataJSON must be base64url(signing_message).
    Passkey {
        credential_id: Vec<u8>,
        /// P-256 public key x-coordinate.
        public_key_x: [u8; 32],
        /// P-256 public key y-coordinate.
        public_key_y: [u8; 32],
        /// WebAuthn Relying Party ID this credential is bound to (e.g.
        /// "wallet.solenchain.io"). The assertion's authenticatorData rpIdHash
        /// must equal SHA-256(rp_id). Empty = unbound (rpId not enforced).
        rp_id: String,
        /// Allowed clientDataJSON origins (e.g. "https://wallet.solenchain.io").
        /// The assertion's origin must be one of these. Empty = origin not enforced.
        origins: Vec<String>,
    },
    Ed25519 { public_key: [u8; 32] },
    Threshold { signers: Vec<[u8; 32]>, threshold: u16 },
    Guardian { guardian_id: AccountId },
    /// Temporary session key with restrictions.
    Session {
        /// Ed25519 session key.
        session_key: [u8; 32],
        /// Block height after which this session expires.
        expires_at: u64,
        /// Maximum spend allowed per single operation (base units). 0 = no
        /// per-op cap.
        spending_limit: u128,
        /// Cumulative lifetime spend cap across every operation signed by this
        /// session key (base units). 0 = unlimited. The running total is tracked
        /// on-chain at `session_spent/{owner_hex}/{session_pk_hex}` and is
        /// incremented only by operations that succeed.
        budget_total: u128,
        /// Allowed contract targets (empty = all allowed).
        allowed_targets: Vec<AccountId>,
        /// Allowed methods (empty = all allowed).
        allowed_methods: Vec<String>,
        /// When true, `allowed_targets`/`allowed_methods` are enforced not just
        /// on the operation's top-level actions but on every contract sub-call
        /// in its execution tree (queued contract→contract calls). Defaults to
        /// false: sub-calls run as the called contract on its own behalf and
        /// cannot touch the owner's funds, so top-level enforcement already
        /// bounds owner exposure; set true for a locked-down agent that must
        /// never transitively trigger a non-allowlisted contract.
        restrict_subcalls: bool,
    },
    /// Post-quantum signature: ML-DSA-65 (FIPS 204). An opt-in, quantum-resistant
    /// account key — Shor's algorithm breaks Ed25519/passkey keys, ML-DSA does
    /// not. The `public_key` is the 1952-byte ML-DSA-65 encoding; the operation's
    /// `signature` carries the 3309-byte ML-DSA signature. Appended last so the
    /// Borsh variant indices of the classical methods above stay stable.
    /// Acceptance is gated by `pq_auth_height` (ships dormant; not the default).
    MlDsa { public_key: Vec<u8> },
    /// True AND-hybrid: an operation must carry BOTH a valid Ed25519 signature
    /// AND a valid ML-DSA-65 signature to authorize. Defense-in-depth for the
    /// post-quantum transition — the account stays secure unless BOTH schemes
    /// are broken. The operation `signature` is `ed25519_sig[64] ‖ ml_dsa_sig`.
    /// Gated by `pq_auth_height` (it relies on ML-DSA verification). Appended
    /// last to keep earlier Borsh variant indices stable.
    Hybrid {
        ed25519_public_key: [u8; 32],
        ml_dsa_public_key: Vec<u8>,
    },
}

/// A smart account (no EOAs in Solen).
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Account {
    pub id: AccountId,
    pub code_hash: Hash,
    pub auth_methods: Vec<AuthMethod>,
    pub nonce: u64,
    pub balance: u128,
}
