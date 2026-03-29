//! Genesis configuration: loads chain parameters, validators, and initial
//! account allocations from a JSON config file.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use solen_crypto::Keypair;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_storage::StateStore;
use solen_types::account::AuthMethod;
use tracing::info;

/// Top-level genesis configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisConfig {
    /// Human-readable chain name.
    pub chain_name: String,
    /// Unique chain identifier.
    pub chain_id: u64,
    /// Block production interval in milliseconds.
    pub block_time_ms: u64,
    /// Blocks per epoch.
    pub epoch_length: u64,
    /// Initial validators.
    pub validators: Vec<ValidatorConfig>,
    /// Initial account allocations.
    pub accounts: Vec<AccountAllocation>,
    /// Faucet configuration (optional).
    pub faucet: Option<FaucetConfig>,
}

/// A validator in the genesis set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorConfig {
    /// Human-readable name.
    pub name: String,
    /// 32-byte seed as hex (testnet only — derives keypair).
    /// For mainnet, use `public_key_hex` instead and keep seeds offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_hex: Option<String>,
    /// 32-byte public key as hex (mainnet — seed stays offline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_key_hex: Option<String>,
    /// Initial stake.
    pub stake: u128,
}

/// An initial account allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountAllocation {
    /// Account name (converted to 32-byte zero-padded ID).
    pub name: String,
    /// Optional 32-byte account ID override as hex. If not set, derived from name.
    pub id_hex: Option<String>,
    /// Initial balance.
    pub balance: u128,
    /// Optional Ed25519 public key hex for auth.
    pub public_key_hex: Option<String>,
    /// Optional seed hex (derives public key from seed).
    pub seed_hex: Option<String>,
}

/// Faucet configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaucetConfig {
    /// Faucet account name.
    pub account_name: String,
    /// Seed hex for the faucet keypair.
    pub seed_hex: String,
    /// Amount to drip per request.
    pub drip_amount: u128,
    /// Cooldown between drips per recipient (seconds).
    pub cooldown_secs: u64,
}

impl GenesisConfig {
    /// Load from a JSON file.
    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read_to_string(path)?;
        let config: Self = serde_json::from_str(&data)?;
        Ok(config)
    }

    /// Save to a JSON file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let data = serde_json::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, data)?;
        Ok(())
    }

    /// Apply this genesis config to a state store.
    pub fn apply(&self, store: &mut dyn StateStore) -> Result<()> {
        let mut genesis_accounts = Vec::new();

        // Add validator accounts.
        for v in &self.validators {
            let public_key = if let Some(seed_hex) = &v.seed_hex {
                // Testnet: derive public key from seed.
                let seed = hex_decode_32(seed_hex)?;
                let kp = Keypair::from_seed(&seed);
                kp.public_key()
            } else if let Some(pk_hex) = &v.public_key_hex {
                // Mainnet: use public key directly.
                hex_decode_32(pk_hex)?
            } else {
                anyhow::bail!("validator '{}' needs either seed_hex or public_key_hex", v.name);
            };

            let id = name_to_id(&v.name);

            genesis_accounts.push(GenesisAccount {
                id,
                balance: v.stake,
                auth_methods: vec![AuthMethod::Ed25519 { public_key }],
            });

            info!(
                name = %v.name,
                id = hex_encode(&id),
                pubkey = hex_encode(&public_key),
                stake = v.stake,
                "genesis validator"
            );
        }

        // Add allocated accounts.
        for a in &self.accounts {
            let id = match &a.id_hex {
                Some(hex) => hex_decode_32(hex)?,
                None => name_to_id(&a.name),
            };

            let auth_methods = if let Some(seed_hex) = &a.seed_hex {
                let seed = hex_decode_32(seed_hex)?;
                let kp = Keypair::from_seed(&seed);
                vec![AuthMethod::Ed25519 {
                    public_key: kp.public_key(),
                }]
            } else if let Some(pk_hex) = &a.public_key_hex {
                let pk = hex_decode_32(pk_hex)?;
                vec![AuthMethod::Ed25519 { public_key: pk }]
            } else {
                vec![]
            };

            genesis_accounts.push(GenesisAccount {
                id,
                balance: a.balance,
                auth_methods,
            });

            info!(
                name = %a.name,
                id = hex_encode(&id),
                balance = a.balance,
                "genesis account"
            );
        }

        // Add faucet account.
        if let Some(faucet) = &self.faucet {
            let seed = hex_decode_32(&faucet.seed_hex)?;
            let kp = Keypair::from_seed(&seed);
            let id = name_to_id(&faucet.account_name);

            genesis_accounts.push(GenesisAccount {
                id,
                balance: 1_000_000_000_000, // 1T tokens for faucet
                auth_methods: vec![AuthMethod::Ed25519 {
                    public_key: kp.public_key(),
                }],
            });

            info!(
                name = %faucet.account_name,
                id = hex_encode(&id),
                drip = faucet.drip_amount,
                "genesis faucet"
            );
        }

        // Add treasury account.
        let treasury_id = name_to_id("treasury");
        genesis_accounts.push(GenesisAccount {
            id: treasury_id,
            balance: 0,
            auth_methods: vec![],
        });

        apply_genesis(store, genesis_accounts)?;

        info!(
            chain_name = %self.chain_name,
            chain_id = self.chain_id,
            validators = self.validators.len(),
            accounts = self.accounts.len(),
            "genesis applied"
        );

        Ok(())
    }

    /// Generate a default devnet config.
    pub fn default_devnet() -> Self {
        Self {
            chain_name: "Solen Devnet".into(),
            chain_id: 1337,
            block_time_ms: 2000,
            epoch_length: 100,
            validators: vec![ValidatorConfig {
                name: "validator-1".into(),
                seed_hex: Some("01".repeat(32)),
                public_key_hex: None,
                stake: 1_000_000,
            }],
            accounts: vec![
                AccountAllocation {
                    name: "alice".into(),
                    id_hex: None,
                    balance: 10_000,
                    public_key_hex: None,
                    seed_hex: Some("0a".repeat(32)),
                },
                AccountAllocation {
                    name: "bob".into(),
                    id_hex: None,
                    balance: 5_000,
                    public_key_hex: None,
                    seed_hex: None,
                },
            ],
            faucet: Some(FaucetConfig {
                account_name: "faucet".into(),
                seed_hex: "2a".repeat(32),
                drip_amount: 10_000,
                cooldown_secs: 60,
            }),
        }
    }

    /// Generate a testnet config.
    pub fn default_testnet() -> Self {
        Self {
            chain_name: "Solen Testnet".into(),
            chain_id: 9000,
            block_time_ms: 2000,
            epoch_length: 100,
            validators: vec![
                ValidatorConfig {
                    name: "validator-1".into(),
                    seed_hex: Some("01".repeat(32)),
                    public_key_hex: None,
                    stake: 1_000_000,
                },
                ValidatorConfig {
                    name: "validator-2".into(),
                    seed_hex: Some("02".repeat(32)),
                    public_key_hex: None,
                    stake: 1_000_000,
                },
                ValidatorConfig {
                    name: "validator-3".into(),
                    seed_hex: Some("03".repeat(32)),
                    public_key_hex: None,
                    stake: 1_000_000,
                },
                ValidatorConfig {
                    name: "validator-4".into(),
                    seed_hex: Some("04".repeat(32)),
                    public_key_hex: None,
                    stake: 1_000_000,
                },
            ],
            accounts: vec![],
            faucet: Some(FaucetConfig {
                account_name: "faucet".into(),
                seed_hex: "2a".repeat(32),
                drip_amount: 100_000,
                cooldown_secs: 300,
            }),
        }
    }
}

fn name_to_id(name: &str) -> [u8; 32] {
    let mut id = [0u8; 32];
    let bytes = name.as_bytes();
    let len = bytes.len().min(32);
    id[..len].copy_from_slice(&bytes[..len]);
    id
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode_32(s: &str) -> Result<[u8; 32]> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes: Vec<u8> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into))
        .collect::<Result<Vec<u8>>>()?;
    if bytes.len() != 32 {
        anyhow::bail!("expected 32 bytes, got {}", bytes.len());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}
