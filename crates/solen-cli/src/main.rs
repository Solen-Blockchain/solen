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
    /// JSON-RPC endpoint URL
    #[arg(long, default_value = "http://127.0.0.1:9944", global = true)]
    rpc: String,

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
        Commands::Transfer { from, to, amount } => {
            commands::cmd_transfer(&rpc, &from, &to, amount).await?
        }
        Commands::Deploy { from, wasm_file } => {
            commands::cmd_deploy(&rpc, &from, &wasm_file).await?
        }
        Commands::Call {
            from,
            target,
            method,
            args,
        } => {
            commands::cmd_call(&rpc, &from, &target, &method, args.as_deref()).await?
        }
        Commands::Key { action } => match action {
            KeyAction::Generate { name } => commands::cmd_key_generate(&name)?,
            KeyAction::Import { name, seed } => commands::cmd_key_import(&name, &seed)?,
            KeyAction::List => commands::cmd_key_list()?,
        },
    }

    Ok(())
}
