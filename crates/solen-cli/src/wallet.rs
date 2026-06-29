//! Local wallet key management with optional password encryption.
//!
//! Keys are stored as JSON in ~/.solen/keys.json.
//! When locked, seeds are AES-256-GCM encrypted with an Argon2id-derived key.

use std::collections::HashMap;
use std::path::PathBuf;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{bail, Result};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use solen_crypto::{Keypair, MlDsaKeypair};
use solen_types::account::AuthMethod;
use solen_types::encoding::account_to_base58;

/// Salt length for Argon2.
const SALT_LEN: usize = 16;
/// Nonce length for AES-256-GCM.
const NONCE_LEN: usize = 12;

// ── Keystore types ─────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Keystore {
    pub keys: HashMap<String, StoredKey>,
    /// Present when the keystore is password-locked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lock: Option<LockMeta>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LockMeta {
    /// Argon2 salt, hex-encoded.
    pub salt_hex: String,
    /// AES-GCM nonce used to encrypt the verification token, hex-encoded.
    pub verify_nonce_hex: String,
    /// Encrypted known plaintext for password verification, hex-encoded.
    pub verify_ciphertext_hex: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredKey {
    pub name: String,
    /// Hex-encoded seed — plaintext when unlocked, absent when locked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_hex: Option<String>,
    pub public_key_hex: String,
    pub account_id_hex: String,
    /// Encrypted seed — present only when locked. Hex-encoded (nonce ++ ciphertext).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_seed_hex: Option<String>,
    /// Signature scheme: absent or "ed25519" = classical (default), "ml-dsa" =
    /// post-quantum ML-DSA-65. The seed format is the same 32 bytes either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
}

/// A loaded signer — classical or post-quantum — built from a stored key's seed
/// and scheme. Used everywhere an operation is signed.
pub enum Signer {
    Ed25519(Keypair),
    MlDsa(MlDsaKeypair),
    /// AND-hybrid: both keys derived from the same 32-byte seed. Breaking
    /// Ed25519 (revealing its scalar, not the seed) leaves the ML-DSA key safe.
    Hybrid(Keypair, MlDsaKeypair),
}

impl Signer {
    /// Sign a message with the appropriate scheme. Ed25519 → 64 bytes,
    /// ML-DSA-65 → 3309 bytes, Hybrid → ed25519[64] ‖ ml_dsa[3309].
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        match self {
            Signer::Ed25519(kp) => kp.sign(message).to_vec(),
            Signer::MlDsa(kp) => kp.sign(message),
            Signer::Hybrid(ed, ml) => {
                let mut sig = ed.sign(message).to_vec();
                sig.extend_from_slice(&ml.sign(message));
                sig
            }
        }
    }

    /// The on-chain auth method this signer corresponds to (for SetAuth).
    pub fn auth_method(&self) -> AuthMethod {
        match self {
            Signer::Ed25519(kp) => AuthMethod::Ed25519 { public_key: kp.public_key() },
            Signer::MlDsa(kp) => AuthMethod::MlDsa { public_key: kp.public_key() },
            Signer::Hybrid(ed, ml) => AuthMethod::Hybrid {
                ed25519_public_key: ed.public_key(),
                ml_dsa_public_key: ml.public_key(),
            },
        }
    }

    pub fn scheme(&self) -> &'static str {
        match self {
            Signer::Ed25519(_) => "ed25519",
            Signer::MlDsa(_) => "ml-dsa",
            Signer::Hybrid(..) => "hybrid",
        }
    }
}

// Backwards compatibility: accept old format where seed_hex is a bare string.
// The custom Deserialize on StoredKey handles both via Option + serde defaults.

// ── Path helpers ───────────────────────────────────────────────

fn keystore_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".solen").join("keys.json")
}

// ── Load / Save ────────────────────────────────────────────────

pub fn load_keystore() -> Result<Keystore> {
    let path = keystore_path();
    if !path.exists() {
        return Ok(Keystore::default());
    }
    let data = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&data)?)
}

pub fn save_keystore(ks: &Keystore) -> Result<()> {
    let path = keystore_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        // Restrict directory permissions to owner only (Unix).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let data = serde_json::to_string_pretty(ks)?;
    std::fs::write(&path, data)?;
    // Restrict file permissions to owner-read/write only (Unix).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn is_locked(ks: &Keystore) -> bool {
    ks.lock.is_some()
}

// ── Key generation / import ────────────────────────────────────

pub fn generate_key(name: &str) -> Result<StoredKey> {
    let seed: [u8; 32] = rand_seed();
    let kp = Keypair::from_seed(&seed);
    let public_key = kp.public_key();

    Ok(StoredKey {
        name: name.to_string(),
        seed_hex: Some(hex_encode(&seed)),
        public_key_hex: account_to_base58(&public_key),
        account_id_hex: account_to_base58(&public_key),
        encrypted_seed_hex: None,
        scheme: None,
    })
}

pub fn import_key(name: &str, seed_hex: &str) -> Result<StoredKey> {
    let seed_bytes = hex_decode(seed_hex)?;
    let mut seed = [0u8; 32];
    if seed_bytes.len() != 32 {
        bail!("seed must be exactly 32 bytes (64 hex chars)");
    }
    seed.copy_from_slice(&seed_bytes);

    let kp = Keypair::from_seed(&seed);
    let public_key = kp.public_key();

    Ok(StoredKey {
        name: name.to_string(),
        seed_hex: Some(hex_encode(&seed)),
        public_key_hex: account_to_base58(&public_key),
        account_id_hex: account_to_base58(&public_key),
        encrypted_seed_hex: None,
        scheme: None,
    })
}

// ── Load keypair (handles locked state) ────────────────────────

pub fn load_keypair(ks: &Keystore, name: &str) -> Result<(Signer, [u8; 32])> {
    let key = ks
        .keys
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("key '{}' not found. Run: solen key generate {}", name, name))?;

    let seed_hex = if let Some(ref s) = key.seed_hex {
        s.clone()
    } else if let Some(ref enc) = key.encrypted_seed_hex {
        // Wallet is locked — prompt for password.
        let lock = ks.lock.as_ref()
            .ok_or_else(|| anyhow::anyhow!("key is encrypted but no lock metadata found"))?;
        let password = prompt_password("Enter wallet password: ")?;
        let derived = derive_key(&password, &hex_decode(&lock.salt_hex)?)?;
        // Verify password against the known token first.
        verify_password(&derived, lock)?;
        // Decrypt the seed.
        decrypt_seed(&derived, enc)?
    } else {
        bail!("key '{}' has no seed (neither plaintext nor encrypted)", name);
    };

    let seed_bytes = hex_decode(&seed_hex)?;
    if seed_bytes.len() != 32 {
        bail!("seed must be exactly 32 bytes");
    }
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&seed_bytes);

    // Build the right signer for the key's scheme. The 32-byte seed format is
    // shared, so a key can be ed25519 (default) or post-quantum ML-DSA-65.
    let signer = match key.scheme.as_deref() {
        Some("ml-dsa") => Signer::MlDsa(MlDsaKeypair::from_seed(&seed)),
        Some("hybrid") => Signer::Hybrid(Keypair::from_seed(&seed), MlDsaKeypair::from_seed(&seed)),
        _ => Signer::Ed25519(Keypair::from_seed(&seed)),
    };

    let account_id = solen_types::encoding::parse_address(&key.account_id_hex)
        .map_err(|e| anyhow::anyhow!("invalid account_id: {}", e))?;

    Ok((signer, account_id))
}

/// Generate a fresh ML-DSA-65 (post-quantum) keypair, returning its 32-byte seed
/// and serialized public key. The caller submits a SetAuth carrying this public
/// key, then calls [`persist_ml_dsa`] ONLY after the on-chain rotation succeeds.
pub fn new_ml_dsa() -> ([u8; 32], Vec<u8>) {
    let mut seed = [0u8; 32];
    solen_crypto::random_bytes(&mut seed);
    let pubkey = MlDsaKeypair::from_seed(&seed).public_key();
    (seed, pubkey)
}

/// Persist a rotated ML-DSA-65 key under `name`, keeping the same `account_id`.
/// Called after the SetAuth confirming the rotation has been accepted on-chain.
pub fn persist_ml_dsa(ks: &mut Keystore, name: &str, seed: &[u8; 32], pubkey: &[u8]) -> Result<()> {
    let key = ks
        .keys
        .get_mut(name)
        .ok_or_else(|| anyhow::anyhow!("key '{}' not found", name))?;
    key.seed_hex = Some(hex_encode(seed));
    key.encrypted_seed_hex = None;
    key.scheme = Some("ml-dsa".to_string());
    key.public_key_hex = hex_encode(pubkey);
    Ok(())
}

/// Generate a fresh AND-hybrid keypair: ONE 32-byte seed deriving both an
/// Ed25519 and an ML-DSA-65 key. Returns (seed, ed25519_public_key,
/// ml_dsa_public_key). Persisted via [`persist_hybrid`] after on-chain SetAuth.
pub fn new_hybrid() -> ([u8; 32], [u8; 32], Vec<u8>) {
    let mut seed = [0u8; 32];
    solen_crypto::random_bytes(&mut seed);
    let ed = Keypair::from_seed(&seed).public_key();
    let ml = MlDsaKeypair::from_seed(&seed).public_key();
    (seed, ed, ml)
}

/// Persist a rotated hybrid key under `name` (same `account_id`).
pub fn persist_hybrid(ks: &mut Keystore, name: &str, seed: &[u8; 32], ed_pubkey: &[u8; 32]) -> Result<()> {
    let key = ks
        .keys
        .get_mut(name)
        .ok_or_else(|| anyhow::anyhow!("key '{}' not found", name))?;
    key.seed_hex = Some(hex_encode(seed));
    key.encrypted_seed_hex = None;
    key.scheme = Some("hybrid".to_string());
    key.public_key_hex = account_to_base58(ed_pubkey);
    Ok(())
}

// ── Lock / Unlock ──────────────────────────────────────────────

/// Encrypt all seeds in the keystore with a password.
pub fn lock_keystore(ks: &mut Keystore, password: &str) -> Result<()> {
    if ks.lock.is_some() {
        bail!("wallet is already locked");
    }
    if ks.keys.is_empty() {
        bail!("no keys to lock");
    }

    // Generate salt and derive key.
    let salt = random_bytes::<SALT_LEN>();
    let derived = derive_key(password, &salt)?;

    // Create verification token (encrypt a known plaintext).
    let verify_nonce = random_bytes::<NONCE_LEN>();
    let verify_ciphertext = encrypt_bytes(&derived, &verify_nonce, b"solen-wallet-v1")?;

    let lock = LockMeta {
        salt_hex: hex_encode(&salt),
        verify_nonce_hex: hex_encode(&verify_nonce),
        verify_ciphertext_hex: hex_encode(&verify_ciphertext),
    };

    // Encrypt each seed.
    for key in ks.keys.values_mut() {
        if let Some(ref seed_hex) = key.seed_hex {
            let nonce = random_bytes::<NONCE_LEN>();
            let ciphertext = encrypt_bytes(&derived, &nonce, seed_hex.as_bytes())?;
            // Store as nonce ++ ciphertext, hex-encoded.
            let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
            combined.extend_from_slice(&nonce);
            combined.extend_from_slice(&ciphertext);
            key.encrypted_seed_hex = Some(hex_encode(&combined));
            key.seed_hex = None;
        }
    }

    ks.lock = Some(lock);
    Ok(())
}

/// Decrypt all seeds in the keystore, removing password protection.
pub fn unlock_keystore(ks: &mut Keystore, password: &str) -> Result<()> {
    let lock = ks.lock.as_ref()
        .ok_or_else(|| anyhow::anyhow!("wallet is not locked"))?
        .clone();

    let salt = hex_decode(&lock.salt_hex)?;
    let derived = derive_key(password, &salt)?;

    // Verify password.
    verify_password(&derived, &lock)?;

    // Decrypt each seed.
    for key in ks.keys.values_mut() {
        if let Some(ref enc) = key.encrypted_seed_hex {
            let seed_hex = decrypt_seed(&derived, enc)?;
            key.seed_hex = Some(seed_hex);
            key.encrypted_seed_hex = None;
        }
    }

    ks.lock = None;
    Ok(())
}

/// Encrypt a single new key to add to an already-locked keystore.
pub fn encrypt_new_key(ks: &Keystore, key: &mut StoredKey, password: &str) -> Result<()> {
    let lock = ks.lock.as_ref()
        .ok_or_else(|| anyhow::anyhow!("wallet is not locked"))?;

    let salt = hex_decode(&lock.salt_hex)?;
    let derived = derive_key(password, &salt)?;

    // Verify password first.
    verify_password(&derived, lock)?;

    if let Some(ref seed_hex) = key.seed_hex {
        let nonce = random_bytes::<NONCE_LEN>();
        let ciphertext = encrypt_bytes(&derived, &nonce, seed_hex.as_bytes())?;
        let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        combined.extend_from_slice(&nonce);
        combined.extend_from_slice(&ciphertext);
        key.encrypted_seed_hex = Some(hex_encode(&combined));
        key.seed_hex = None;
    }

    Ok(())
}

/// Change the wallet password (must already be locked).
pub fn change_password(ks: &mut Keystore, old_password: &str, new_password: &str) -> Result<()> {
    // First unlock with old password.
    unlock_keystore(ks, old_password)?;
    // Then re-lock with new password.
    lock_keystore(ks, new_password)?;
    Ok(())
}

// ── Crypto helpers ─────────────────────────────────────────────

fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow::anyhow!("key derivation failed: {}", e))?;
    Ok(key)
}

fn encrypt_bytes(key: &[u8; 32], nonce: &[u8; NONCE_LEN], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(key.into());
    let nonce = Nonce::from_slice(nonce);
    cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("encryption failed: {}", e))
}

fn decrypt_bytes(key: &[u8; 32], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(key.into());
    let nonce = Nonce::from_slice(nonce);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!("decryption failed — wrong password?"))
}

fn verify_password(derived: &[u8; 32], lock: &LockMeta) -> Result<()> {
    let nonce = hex_decode(&lock.verify_nonce_hex)?;
    let ciphertext = hex_decode(&lock.verify_ciphertext_hex)?;
    let plaintext = decrypt_bytes(derived, &nonce, &ciphertext)?;
    if plaintext != b"solen-wallet-v1" {
        bail!("incorrect password");
    }
    Ok(())
}

fn decrypt_seed(derived: &[u8; 32], encrypted_hex: &str) -> Result<String> {
    let combined = hex_decode(encrypted_hex)?;
    if combined.len() < NONCE_LEN + 1 {
        bail!("encrypted seed data is too short");
    }
    let (nonce, ciphertext) = combined.split_at(NONCE_LEN);
    let plaintext = decrypt_bytes(derived, nonce, ciphertext)?;
    Ok(String::from_utf8(plaintext)?)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    use rand::RngCore;
    let mut buf = [0u8; N];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

/// Prompt user for a password (hidden input).
pub fn prompt_password(prompt: &str) -> Result<String> {
    let pass = rpassword::prompt_password(prompt)?;
    if pass.is_empty() {
        bail!("password cannot be empty");
    }
    Ok(pass)
}

/// Prompt for a new password with confirmation.
pub fn prompt_new_password() -> Result<String> {
    let pass = rpassword::prompt_password("New wallet password: ")?;
    if pass.is_empty() {
        bail!("password cannot be empty");
    }
    if pass.len() < 8 {
        bail!("password must be at least 8 characters");
    }
    let confirm = rpassword::prompt_password("Confirm password: ")?;
    if pass != confirm {
        bail!("passwords do not match");
    }
    Ok(pass)
}

// ── Utility ────────────────────────────────────────────────────

pub fn name_to_account_id(name: &str) -> [u8; 32] {
    let mut id = [0u8; 32];
    let bytes = name.as_bytes();
    let len = bytes.len().min(32);
    id[..len].copy_from_slice(&bytes[..len]);
    id
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| anyhow::anyhow!("invalid hex at position {}: {}", i, e))
        })
        .collect()
}

fn rand_seed() -> [u8; 32] {
    // Use cryptographically secure random when available.
    random_bytes::<32>()
}
