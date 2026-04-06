//! Solen CLI — interact with the Solen network from the command line.
//!
//! Usage:
//!   solen status                          Show chain status
//!   solen balance <account>               Get account balance
//!   solen account <account>               Get account details
//!   solen block [height]                  Get block (latest if no height)
//!   solen transfer <from> <to> <amount>   Send tokens
//!   solen deploy <from> <wasm-file>       Deploy a contract
//!   solen call <from> <target> <method>   Call a contract
//!   solen key generate <name>             Generate a new keypair
//!   solen key import <name> <seed-hex>    Import a keypair from seed
//!   solen key list                        List stored keys
//!   solen key lock                        Lock wallet with a password
//!   solen key unlock                      Unlock wallet (remove password)
//!   solen key change-password             Change wallet password

mod commands;
mod rpc;
mod wallet;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "solen", version = "0.1.0", about = "Solen CLI — interact with the Solen network")]
struct Cli {
    /// JSON-RPC endpoint URL (overrides --network default)
    #[arg(long, global = true)]
    rpc: Option<String>,

    /// Chain ID for transaction signing (overrides --network default)
    #[arg(long, global = true)]
    chain_id: Option<u64>,

    /// Network preset: devnet, testnet, or mainnet.
    /// Sets default RPC endpoint and chain ID.
    #[arg(long, global = true)]
    network: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show chain status
    Status,

    /// Get account balance
    Balance {
        /// Account name or hex ID
        account: String,
    },

    /// Get account details
    Account {
        /// Account name or hex ID
        account: String,
    },

    /// Get block info
    Block {
        /// Block height (latest if omitted)
        height: Option<u64>,
    },

    /// List all validators and their stake
    Validators,

    /// Claim vested tokens
    ClaimVesting {
        /// Your key name
        from: String,
    },

    /// Propose changing the block time (governance)
    ProposeBlockTime {
        /// Your key name
        from: String,
        /// New block time in milliseconds
        new_block_time_ms: u64,
        /// Description of the proposal
        description: String,
    },

    /// Propose changing the minimum validator stake (governance)
    ProposeMinStake {
        /// Your key name
        from: String,
        /// New minimum stake in SOLEN (e.g., 15000)
        new_min_stake: String,
        /// Description of the proposal
        description: String,
    },

    /// Vote on a governance proposal
    Vote {
        /// Your key name
        from: String,
        /// Proposal ID
        proposal_id: u64,
        /// Vote yes or no
        #[arg(long)]
        yes: bool,
        /// Stake weight for the vote
        #[arg(long, default_value = "1")]
        weight: String,
    },

    /// Finalize a governance proposal after voting period
    FinalizeProposal {
        /// Your key name
        from: String,
        /// Proposal ID
        proposal_id: u64,
    },

    /// Execute a passed governance proposal after timelock
    ExecuteProposal {
        /// Your key name
        from: String,
        /// Proposal ID
        proposal_id: u64,
    },

    /// Register as a new validator with self-stake (min 500,000 SOLEN)
    RegisterValidator {
        /// Your key name
        from: String,
        /// Amount in SOLEN (e.g., 500000 or 500000.5)
        amount: String,
    },

    /// Delegate tokens to a validator
    Stake {
        /// Your key name
        from: String,
        /// Validator address (hex)
        validator: String,
        /// Amount in SOLEN (e.g., 1000 or 1000.5)
        amount: String,
    },

    /// Undelegate tokens from a validator
    Unstake {
        /// Your key name
        from: String,
        /// Validator address (hex)
        validator: String,
        /// Amount in SOLEN (e.g., 1000 or 1000.5)
        amount: String,
    },

    /// Withdraw matured unstaked tokens
    WithdrawStake {
        /// Your key name
        from: String,
    },

    /// Set the vesting admin account (one-time setup or transfer)
    SetVestingAdmin {
        /// Your key name (current admin or first-time setter)
        from: String,
        /// New admin account name or address
        new_admin: String,
    },

    /// Add a vesting schedule for a recipient (admin only)
    AddVesting {
        /// Your key name (must be vesting admin)
        from: String,
        /// Recipient account name or address
        recipient: String,
        /// Amount in SOLEN (e.g., 100000)
        amount: String,
        /// Vesting type: team, investor, validator, or custom
        #[arg(long, default_value = "investor")]
        vesting_type: String,
        /// Custom cliff in months (only for --vesting-type custom)
        #[arg(long)]
        cliff_months: Option<u64>,
        /// Custom total vest in months (only for --vesting-type custom)
        #[arg(long)]
        vest_months: Option<u64>,
    },

    /// Reactivate a jailed validator after downtime slash
    Unjail {
        /// Your validator key name
        from: String,
    },

    /// Transfer tokens between accounts
    Transfer {
        /// Sender key name (must exist in keystore)
        from: String,
        /// Recipient name or hex ID
        to: String,
        /// Amount in SOLEN (e.g., 100 or 100.5)
        amount: String,
    },

    /// Deploy a WASM contract
    Deploy {
        /// Deployer key name
        from: String,
        /// Path to .wasm file
        #[arg(name = "wasm-file")]
        wasm_file: String,
    },

    /// Call a contract method
    Call {
        /// Caller key name
        from: String,
        /// Target contract name or hex ID
        target: String,
        /// Method name
        method: String,
        /// Arguments as hex bytes (optional)
        #[arg(long)]
        args: Option<String>,
    },

    /// Register a rollup domain on L1 (requires 10,000 SOLEN deposit)
    RegisterRollup {
        /// Your key name
        from: String,
        /// Rollup ID (numeric)
        rollup_id: u64,
        /// Rollup name
        name: String,
        /// Proof type (e.g., "mock", "validity", "fraud")
        #[arg(long, default_value = "mock")]
        proof_type: String,
        /// Genesis state root (64-char hex, defaults to zero)
        #[arg(long, default_value = "0000000000000000000000000000000000000000000000000000000000000000")]
        genesis_state_root: String,
    },

    /// Register a deployed contract as a paymaster (fee sponsor)
    RegisterPaymaster {
        /// Contract key name (must be a deployed contract)
        from: String,
    },

    /// Unregister a contract as a paymaster
    UnregisterPaymaster {
        /// Contract key name
        from: String,
    },

    /// Initiate guardian recovery for a lost account (sender must be a guardian)
    InitiateRecovery {
        /// Your key name (must be a guardian of the target account)
        from: String,
        /// Target account to recover (hex)
        target: String,
        /// New public key for the recovered account (hex, 64 chars)
        new_public_key: String,
    },

    /// Confirm a pending guardian recovery (sender must be a guardian)
    ConfirmRecovery {
        /// Your key name (must be a guardian of the target account)
        from: String,
        /// Recovery ID
        recovery_id: u64,
    },

    /// Cancel a pending recovery (sender must be the account owner)
    CancelRecovery {
        /// Your key name (the account being recovered)
        from: String,
        /// Recovery ID
        recovery_id: u64,
    },

    /// Execute a recovery after timelock expires
    ExecuteRecovery {
        /// Your key name (anyone can execute after timelock + confirmations)
        from: String,
        /// Recovery ID
        recovery_id: u64,
    },

    /// Convert an account to multi-sig (threshold signing)
    Multisig {
        /// Account key name (current owner)
        from: String,
        /// Threshold (number of required signatures)
        #[arg(long, short)]
        threshold: u16,
        /// Signer public keys (hex), comma-separated
        #[arg(long, short, value_delimiter = ',')]
        signers: Vec<String>,
    },

    /// Key management
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
}

#[derive(Subcommand)]
enum KeyAction {
    /// Generate a new keypair
    Generate {
        /// Name for the key
        name: String,
    },
    /// Import a keypair from a 32-byte hex seed
    Import {
        /// Name for the key
        name: String,
        /// 32-byte seed as hex (64 chars)
        seed: String,
    },
    /// List all stored keys
    List,
    /// Lock the wallet with a password (encrypts all seeds)
    Lock,
    /// Unlock the wallet (decrypts all seeds, removes password)
    Unlock,
    /// Change the wallet password
    ChangePassword,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Resolve network defaults. Explicit --rpc and --chain-id override.
    let (default_rpc, default_chain_id) = match cli.network.as_deref() {
        Some("mainnet") => ("https://rpc.solenchain.io", 1u64),
        Some("testnet") => ("https://testnet-rpc.solenchain.io", 9000u64),
        Some("devnet") | None => ("http://127.0.0.1:29944", 1337u64),
        Some(other) => anyhow::bail!("unknown network '{}' (use devnet, testnet, or mainnet)", other),
    };

    let rpc_url = cli.rpc.as_deref().unwrap_or(default_rpc);
    let chain_id = cli.chain_id.unwrap_or(default_chain_id);

    let rpc = rpc::RpcClient::new(rpc_url);

    match cli.command {
        Commands::Status => commands::cmd_status(&rpc).await?,
        Commands::Balance { account } => commands::cmd_balance(&rpc, &account).await?,
        Commands::Account { account } => commands::cmd_account(&rpc, &account).await?,
        Commands::Block { height } => commands::cmd_block(&rpc, height).await?,
        Commands::Validators => commands::cmd_validators(&rpc).await?,
        Commands::ClaimVesting { from } => {
            commands::cmd_claim_vesting(&rpc, &from, chain_id).await?
        }
        Commands::ProposeBlockTime { from, new_block_time_ms, description } => {
            commands::cmd_propose_block_time(&rpc, &from, new_block_time_ms, &description, chain_id).await?
        }
        Commands::ProposeMinStake { from, new_min_stake, description } => {
            let base = parse_solen_amount(&new_min_stake)?;
            commands::cmd_propose_min_stake(&rpc, &from, base, &description, chain_id).await?
        }
        Commands::Vote { from, proposal_id, yes, weight } => {
            let base = parse_solen_amount(&weight)?;
            commands::cmd_vote(&rpc, &from, proposal_id, yes, base, chain_id).await?
        }
        Commands::FinalizeProposal { from, proposal_id } => {
            commands::cmd_finalize_proposal(&rpc, &from, proposal_id, chain_id).await?
        }
        Commands::ExecuteProposal { from, proposal_id } => {
            commands::cmd_execute_proposal(&rpc, &from, proposal_id, chain_id).await?
        }
        Commands::RegisterValidator { from, amount } => {
            let base = parse_solen_amount(&amount)?;
            commands::cmd_register_validator(&rpc, &from, base, chain_id).await?
        }
        Commands::Stake { from, validator, amount } => {
            let base = parse_solen_amount(&amount)?;
            commands::cmd_stake(&rpc, &from, &validator, base, chain_id).await?
        }
        Commands::Unstake { from, validator, amount } => {
            let base = parse_solen_amount(&amount)?;
            commands::cmd_unstake(&rpc, &from, &validator, base, chain_id).await?
        }
        Commands::WithdrawStake { from } => {
            commands::cmd_withdraw_stake(&rpc, &from, chain_id).await?
        }
        Commands::SetVestingAdmin { from, new_admin } => {
            commands::cmd_set_vesting_admin(&rpc, &from, &new_admin, chain_id).await?
        }
        Commands::AddVesting { from, recipient, amount, vesting_type, cliff_months, vest_months } => {
            let base = parse_solen_amount(&amount)?;
            commands::cmd_add_vesting(&rpc, &from, &recipient, base, &vesting_type, cliff_months, vest_months, chain_id).await?
        }
        Commands::Unjail { from } => {
            commands::cmd_unjail(&rpc, &from, chain_id).await?
        }
        Commands::Transfer { from, to, amount } => {
            let base = parse_solen_amount(&amount)?;
            commands::cmd_transfer(&rpc, &from, &to, base, chain_id).await?
        }
        Commands::Deploy { from, wasm_file } => {
            commands::cmd_deploy(&rpc, &from, &wasm_file, chain_id).await?
        }
        Commands::Call {
            from,
            target,
            method,
            args,
        } => {
            commands::cmd_call(&rpc, &from, &target, &method, args.as_deref(), chain_id).await?
        }
        Commands::InitiateRecovery { from, target, new_public_key } => {
            commands::cmd_initiate_recovery(&rpc, &from, &target, &new_public_key, chain_id).await?
        }
        Commands::ConfirmRecovery { from, recovery_id } => {
            commands::cmd_confirm_recovery(&rpc, &from, recovery_id, chain_id).await?
        }
        Commands::CancelRecovery { from, recovery_id } => {
            commands::cmd_cancel_recovery(&rpc, &from, recovery_id, chain_id).await?
        }
        Commands::ExecuteRecovery { from, recovery_id } => {
            commands::cmd_execute_recovery(&rpc, &from, recovery_id, chain_id).await?
        }
        Commands::RegisterRollup { from, rollup_id, name, proof_type, genesis_state_root } => {
            commands::cmd_register_rollup(&rpc, &from, rollup_id, &name, &proof_type, &genesis_state_root, chain_id).await?
        }
        Commands::RegisterPaymaster { from } => {
            commands::cmd_register_paymaster(&rpc, &from, chain_id).await?
        }
        Commands::UnregisterPaymaster { from } => {
            commands::cmd_unregister_paymaster(&rpc, &from, chain_id).await?
        }
        Commands::Multisig { from, threshold, signers } => {
            commands::cmd_multisig(&rpc, &from, threshold, &signers, chain_id).await?
        }
        Commands::Key { action } => match action {
            KeyAction::Generate { name } => commands::cmd_key_generate(&name)?,
            KeyAction::Import { name, seed } => commands::cmd_key_import(&name, &seed)?,
            KeyAction::List => commands::cmd_key_list()?,
            KeyAction::Lock => commands::cmd_key_lock()?,
            KeyAction::Unlock => commands::cmd_key_unlock()?,
            KeyAction::ChangePassword => commands::cmd_key_change_password()?,
        },
    }

    Ok(())
}

/// Parse a SOLEN amount (e.g., "500000" or "100.5") to base units (u128).
/// 1 SOLEN = 100,000,000 base units (8 decimals).
fn parse_solen_amount(s: &str) -> anyhow::Result<u128> {
    const DECIMALS: u32 = 8;
    let multiplier = 10u128.pow(DECIMALS);

    if let Some(dot) = s.find('.') {
        let whole: u128 = s[..dot].parse().map_err(|_| anyhow::anyhow!("invalid amount"))?;
        let frac_str = &s[dot + 1..];
        let frac_len = frac_str.len();
        if frac_len > DECIMALS as usize {
            anyhow::bail!("too many decimal places (max {})", DECIMALS);
        }
        let frac: u128 = frac_str.parse().map_err(|_| anyhow::anyhow!("invalid amount"))?;
        let frac_multiplier = 10u128.pow(DECIMALS - frac_len as u32);
        Ok(whole * multiplier + frac * frac_multiplier)
    } else {
        let whole: u128 = s.parse().map_err(|_| anyhow::anyhow!("invalid amount"))?;
        Ok(whole * multiplier)
    }
}
