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
    /// Team vesting recipients.
    #[serde(default)]
    pub team_vesting: Vec<VestingRecipient>,
    /// Investor vesting recipients.
    #[serde(default)]
    pub investor_vesting: Vec<VestingRecipient>,
    /// Governance voting period in epochs (default: 14 for testnet, 604800 for mainnet ~14 days).
    #[serde(default)]
    pub governance_voting_period: u64,
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

/// A vesting recipient in the genesis config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VestingRecipient {
    pub name: String,
    pub public_key_hex: String,
    pub amount: u128,
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
    ///
    /// Account IDs are derived from public keys. The account address
    /// IS the Ed25519 public key — no hashing, no name-based derivation.
    pub fn apply(&self, store: &mut dyn StateStore) -> Result<()> {
        let mut genesis_accounts = Vec::new();

        // Add validator accounts. Account ID = public key.
        for v in &self.validators {
            let public_key = resolve_validator_pubkey(v)?;

            // Account ID IS the public key.
            let id = public_key;

            genesis_accounts.push(GenesisAccount {
                id,
                balance: v.stake,
                auth_methods: vec![AuthMethod::Ed25519 { public_key }],
            });

            info!(
                name = %v.name,
                address = hex_encode(&id),
                stake = v.stake,
                "genesis validator"
            );
        }

        // Add allocated accounts.
        for a in &self.accounts {
            let (public_key, auth_methods) = if let Some(seed_hex) = &a.seed_hex {
                let seed = hex_decode_32(seed_hex)?;
                let kp = Keypair::from_seed(&seed);
                let pk = kp.public_key();
                (pk, vec![AuthMethod::Ed25519 { public_key: pk }])
            } else if let Some(pk_hex) = &a.public_key_hex {
                let pk = hex_decode_32(pk_hex)?;
                (pk, vec![AuthMethod::Ed25519 { public_key: pk }])
            } else if let Some(id_hex) = &a.id_hex {
                // Explicit ID with no auth (e.g., treasury).
                let id = hex_decode_32(id_hex)?;
                genesis_accounts.push(GenesisAccount {
                    id,
                    balance: a.balance,
                    auth_methods: vec![],
                });
                info!(name = %a.name, address = hex_encode(&id), balance = a.balance, "genesis account (no auth)");
                continue;
            } else {
                anyhow::bail!("account '{}' needs seed_hex, public_key_hex, or id_hex", a.name);
            };

            // Account ID IS the public key.
            let id = public_key;

            genesis_accounts.push(GenesisAccount {
                id,
                balance: a.balance,
                auth_methods,
            });

            info!(
                name = %a.name,
                address = hex_encode(&id),
                balance = a.balance,
                "genesis account"
            );
        }

        // Add faucet account. Account ID = faucet's public key.
        if let Some(faucet) = &self.faucet {
            let seed = hex_decode_32(&faucet.seed_hex)?;
            let kp = Keypair::from_seed(&seed);
            let public_key = kp.public_key();
            let id = public_key; // Account ID IS the public key.

            genesis_accounts.push(GenesisAccount {
                id,
                balance: 1_000_000_000_000_000, // 10M SOLEN for testnet faucet
                auth_methods: vec![AuthMethod::Ed25519 { public_key }],
            });

            info!(
                name = %faucet.account_name,
                address = hex_encode(&id),
                drip = faucet.drip_amount,
                "genesis faucet"
            );
        }

        // ── Fund accounts (tokenomics allocations) ──

        use solen_types::system::*;
        let d = 100_000_000u128; // 10^8 decimals

        // Treasury: 400M SOLEN
        genesis_accounts.push(GenesisAccount {
            id: TREASURY_ADDRESS,
            balance: 400_000_000 * d,
            auth_methods: vec![],
        });

        // Staking rewards pool: 500M SOLEN
        genesis_accounts.push(GenesisAccount {
            id: STAKING_POOL_ADDRESS,
            balance: 500_000_000 * d,
            auth_methods: vec![],
        });

        // Ecosystem fund: 300M SOLEN (available immediately)
        genesis_accounts.push(GenesisAccount {
            id: ECOSYSTEM_FUND_ADDRESS,
            balance: 300_000_000 * d,
            auth_methods: vec![],
        });

        // Community & airdrops: 200M SOLEN (available immediately)
        genesis_accounts.push(GenesisAccount {
            id: COMMUNITY_ADDRESS,
            balance: 200_000_000 * d,
            auth_methods: vec![],
        });

        // Liquidity & market making: 100M SOLEN (available immediately)
        genesis_accounts.push(GenesisAccount {
            id: LIQUIDITY_ADDRESS,
            balance: 100_000_000 * d,
            auth_methods: vec![],
        });

        // Team pool: 300M SOLEN (held by vesting contract)
        genesis_accounts.push(GenesisAccount {
            id: TEAM_POOL_ADDRESS,
            balance: 300_000_000 * d,
            auth_methods: vec![],
        });

        // Investor pool: 100M SOLEN (held by vesting contract)
        genesis_accounts.push(GenesisAccount {
            id: INVESTOR_POOL_ADDRESS,
            balance: 100_000_000 * d,
            auth_methods: vec![],
        });

        info!(
            treasury = 400_000_000u64,
            staking_pool = 500_000_000u64,
            ecosystem = 300_000_000u64,
            community = 200_000_000u64,
            liquidity = 100_000_000u64,
            team_vesting = 300_000_000u64,
            investor_vesting = 100_000_000u64,
            "fund accounts initialized"
        );

        // Compute and store total supply before applying genesis.
        let total_supply: u128 = genesis_accounts.iter().map(|a| a.balance).sum();
        let _ = store.put(b"__total_supply__", &total_supply.to_le_bytes());
        info!(total_supply, "total supply stored");

        apply_genesis(store, genesis_accounts)?;

        // Initialize staking contract with genesis validators.
        let mut staking = solen_system_contracts::staking::StakingContract::new();
        for v in &self.validators {
            let public_key = resolve_validator_pubkey(v)?;
            let _ = staking.register_genesis_validator(public_key, v.stake);
        }
        staking.save(store);

        // Initialize vesting contract with team and investor schedules.
        let mut vesting = solen_system_contracts::vesting::VestingContract::new();
        for r in &self.team_vesting {
            let pk = hex_decode_32(&r.public_key_hex)?;
            vesting.add_schedule(
                pk,
                r.amount,
                solen_system_contracts::vesting::VestingType::Team,
                0, // starts at genesis
            );
            info!(name = %r.name, amount = r.amount, "team vesting schedule");
        }
        for r in &self.investor_vesting {
            let pk = hex_decode_32(&r.public_key_hex)?;
            vesting.add_schedule(
                pk,
                r.amount,
                solen_system_contracts::vesting::VestingType::Investor,
                0,
            );
            info!(name = %r.name, amount = r.amount, "investor vesting schedule");
        }
        vesting.save(store);

        // Initialize governance contract with voting period.
        if self.governance_voting_period > 0 {
            let mut gov = solen_system_contracts::governance::GovernanceContract::new();
            gov.voting_period = self.governance_voting_period;
            gov.save(store);
            info!(voting_period = self.governance_voting_period, "governance initialized");
        }

        info!(
            chain_name = %self.chain_name,
            chain_id = self.chain_id,
            validators = self.validators.len(),
            accounts = self.accounts.len(),
            team_vesting = self.team_vesting.len(),
            investor_vesting = self.investor_vesting.len(),
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
                stake: 5_000_000_000_000,
            }],
            accounts: vec![
                AccountAllocation {
                    name: "alice".into(),
                    id_hex: None,
                    balance: 1_000_000 * 100_000_000, // 1M SOLEN
                    public_key_hex: None,
                    seed_hex: Some("0a".repeat(32)),
                },
                AccountAllocation {
                    name: "bob".into(),
                    id_hex: None,
                    balance: 500_000 * 100_000_000, // 500K SOLEN
                    public_key_hex: None,
                    seed_hex: Some("0b".repeat(32)),
                },
            ],
            faucet: Some(FaucetConfig {
                account_name: "faucet".into(),
                seed_hex: "2a".repeat(32),
                drip_amount: 100_000 * 100_000_000, // 100K SOLEN per drip
                cooldown_secs: 60,
            }),
            team_vesting: vec![],
            investor_vesting: vec![],
            governance_voting_period: 14, // ~46 min for dev testing
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
                    stake: 2_500_000_000_000_000,
                },
                ValidatorConfig {
                    name: "validator-2".into(),
                    seed_hex: Some("02".repeat(32)),
                    public_key_hex: None,
                    stake: 2_500_000_000_000_000,
                },
                ValidatorConfig {
                    name: "validator-3".into(),
                    seed_hex: Some("03".repeat(32)),
                    public_key_hex: None,
                    stake: 2_500_000_000_000_000,
                },
                ValidatorConfig {
                    name: "validator-4".into(),
                    seed_hex: Some("04".repeat(32)),
                    public_key_hex: None,
                    stake: 2_500_000_000_000_000,
                },
            ],
            accounts: vec![],
            faucet: Some(FaucetConfig {
                account_name: "faucet".into(),
                seed_hex: "2a".repeat(32),
                drip_amount: 100_000_000, // 1 SOLEN (8 decimals)
                cooldown_secs: 300,
            }),
            team_vesting: vec![
                VestingRecipient {
                    name: "team-member-1".into(),
                    public_key_hex: "aa".repeat(32),
                    amount: 15_000_000_000_000_000, // 150M SOLEN
                },
                VestingRecipient {
                    name: "team-member-2".into(),
                    public_key_hex: "bb".repeat(32),
                    amount: 15_000_000_000_000_000, // 150M SOLEN
                },
            ],
            investor_vesting: vec![
                VestingRecipient {
                    name: "investor-1".into(),
                    public_key_hex: "cc".repeat(32),
                    amount: 5_000_000_000_000_000, // 50M SOLEN
                },
                VestingRecipient {
                    name: "investor-2".into(),
                    public_key_hex: "dd".repeat(32),
                    amount: 5_000_000_000_000_000, // 50M SOLEN
                },
            ],
            governance_voting_period: 14, // ~46 min for testnet testing
        }
    }
}

/// Resolve a validator's public key from its config.
pub fn resolve_validator_pubkey(v: &ValidatorConfig) -> Result<[u8; 32]> {
    if let Some(seed_hex) = &v.seed_hex {
        let seed = hex_decode_32(seed_hex)?;
        let kp = Keypair::from_seed(&seed);
        Ok(kp.public_key())
    } else if let Some(pk_hex) = &v.public_key_hex {
        hex_decode_32(pk_hex)
    } else {
        anyhow::bail!("validator '{}' needs either seed_hex or public_key_hex", v.name)
    }
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
