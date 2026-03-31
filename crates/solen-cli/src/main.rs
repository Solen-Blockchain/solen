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

mod commands;
mod rpc;
mod wallet;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "solen", version = "0.1.0", about = "Solen CLI — interact with the Solen network")]
struct Cli {
    /// JSON-RPC endpoint URL (default: devnet port)
    #[arg(long, default_value = "http://127.0.0.1:29944", global = true)]
    rpc: String,

    /// Chain ID for transaction signing (devnet=1337, testnet=9000)
    #[arg(long, default_value = "1337", global = true)]
    chain_id: u64,

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

    /// Register as a new validator with self-stake (min 500,000 SOLEN)
    RegisterValidator {
        /// Your key name
        from: String,
        /// Amount to stake
        amount: u128,
    },

    /// Delegate tokens to a validator
    Stake {
        /// Your key name
        from: String,
        /// Validator address (hex)
        validator: String,
        /// Amount to stake
        amount: u128,
    },

    /// Undelegate tokens from a validator
    Unstake {
        /// Your key name
        from: String,
        /// Validator address (hex)
        validator: String,
        /// Amount to unstake
        amount: u128,
    },

    /// Transfer tokens between accounts
    Transfer {
        /// Sender key name (must exist in keystore)
        from: String,
        /// Recipient name or hex ID
        to: String,
        /// Amount to transfer
        amount: u128,
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let rpc = rpc::RpcClient::new(&cli.rpc);

    match cli.command {
        Commands::Status => commands::cmd_status(&rpc).await?,
        Commands::Balance { account } => commands::cmd_balance(&rpc, &account).await?,
        Commands::Account { account } => commands::cmd_account(&rpc, &account).await?,
        Commands::Block { height } => commands::cmd_block(&rpc, height).await?,
        Commands::Validators => commands::cmd_validators(&rpc).await?,
        Commands::ClaimVesting { from } => {
            commands::cmd_claim_vesting(&rpc, &from, cli.chain_id).await?
        }
        Commands::RegisterValidator { from, amount } => {
            commands::cmd_register_validator(&rpc, &from, amount, cli.chain_id).await?
        }
        Commands::Stake { from, validator, amount } => {
            commands::cmd_stake(&rpc, &from, &validator, amount, cli.chain_id).await?
        }
        Commands::Unstake { from, validator, amount } => {
            commands::cmd_unstake(&rpc, &from, &validator, amount, cli.chain_id).await?
        }
        Commands::Transfer { from, to, amount } => {
            commands::cmd_transfer(&rpc, &from, &to, amount, cli.chain_id).await?
        }
        Commands::Deploy { from, wasm_file } => {
            commands::cmd_deploy(&rpc, &from, &wasm_file, cli.chain_id).await?
        }
        Commands::Call {
            from,
            target,
            method,
            args,
        } => {
            commands::cmd_call(&rpc, &from, &target, &method, args.as_deref(), cli.chain_id).await?
        }
        Commands::Multisig { from, threshold, signers } => {
            commands::cmd_multisig(&rpc, &from, threshold, &signers, cli.chain_id).await?
        }
        Commands::Key { action } => match action {
            KeyAction::Generate { name } => commands::cmd_key_generate(&name)?,
            KeyAction::Import { name, seed } => commands::cmd_key_import(&name, &seed)?,
            KeyAction::List => commands::cmd_key_list()?,
        },
    }

    Ok(())
}
