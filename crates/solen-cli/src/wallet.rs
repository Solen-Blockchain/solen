//! Local wallet key management.
//!
//! Keys are stored as JSON in ~/.solen/keys.json.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use solen_crypto::Keypair;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Keystore {
    pub keys: HashMap<String, StoredKey>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredKey {
    pub name: String,
    pub seed_hex: String,
    pub public_key_hex: String,
    pub account_id_hex: String,
}

fn keystore_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".solen").join("keys.json")
}

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
    }
    let data = serde_json::to_string_pretty(ks)?;
    std::fs::write(&path, data)?;
    Ok(())
}

pub fn generate_key(name: &str) -> Result<StoredKey> {
    let seed: [u8; 32] = rand_seed();
    let kp = Keypair::from_seed(&seed);
    let public_key = kp.public_key();

    // Account ID = name padded to 32 bytes (for devnet friendliness)
    let account_id = name_to_account_id(name);

    Ok(StoredKey {
        name: name.to_string(),
        seed_hex: hex_encode(&seed),
        public_key_hex: hex_encode(&public_key),
        account_id_hex: hex_encode(&account_id),
    })
}

pub fn import_key(name: &str, seed_hex: &str) -> Result<StoredKey> {
    let seed_bytes = hex_decode(seed_hex)?;
    let mut seed = [0u8; 32];
    if seed_bytes.len() != 32 {
        anyhow::bail!("seed must be exactly 32 bytes (64 hex chars)");
    }
    seed.copy_from_slice(&seed_bytes);

    let kp = Keypair::from_seed(&seed);
    let account_id = name_to_account_id(name);

    Ok(StoredKey {
        name: name.to_string(),
        seed_hex: hex_encode(&seed),
        public_key_hex: hex_encode(&kp.public_key()),
        account_id_hex: hex_encode(&account_id),
    })
}

pub fn load_keypair(ks: &Keystore, name: &str) -> Result<(Keypair, [u8; 32])> {
    let key = ks
        .keys
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("key '{}' not found. Run: solen key generate {}", name, name))?;

    let seed_bytes = hex_decode(&key.seed_hex)?;
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&seed_bytes);
    let kp = Keypair::from_seed(&seed);

    let account_id_bytes = hex_decode(&key.account_id_hex)?;
    let mut account_id = [0u8; 32];
    account_id.copy_from_slice(&account_id_bytes);

    Ok((kp, account_id))
}

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
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut seed = [0u8; 32];
    // Mix timestamp with process info for basic randomness.
    // Not cryptographically secure — production should use OsRng.
    let hash = solen_crypto::blake3_hash(&nanos.to_le_bytes());
    seed.copy_from_slice(&hash);
    seed
}
