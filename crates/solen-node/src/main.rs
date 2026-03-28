//! Solen node entrypoint.
//!
//! Wires together consensus, execution, networking, and RPC.

use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use solen_consensus::engine::{ConsensusEngine, EngineConfig};
use solen_consensus::mempool::Mempool;
use solen_crypto::Keypair;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_p2p::messages::NetworkMessage;
use solen_p2p::network::{NetworkConfig, NetworkService};
use solen_rpc::server::start_rpc_server;
use solen_storage::StateStore;
use solen_types::account::AuthMethod;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Solen blockchain node.
#[derive(Parser)]
#[command(name = "solen-node", version = "0.1.0")]
struct Cli {
    /// RPC server listen port.
    #[arg(long, default_value = "9944")]
    rpc_port: u16,

    /// P2P listen port.
    #[arg(long, default_value = "30333")]
    p2p_port: u16,

    /// Data directory for persistent storage.
    #[arg(long, default_value = "data/solen-db")]
    data_dir: String,

    /// Block production interval in milliseconds.
    #[arg(long, default_value = "2000")]
    block_time: u64,

    /// Bootstrap peer multiaddrs (can be repeated).
    #[arg(long)]
    bootstrap: Vec<String>,

    /// Validator seed (32 hex bytes). If not set, uses a default devnet key.
    #[arg(long)]
    validator_seed: Option<String>,

    /// Disable P2P networking (single-node mode).
    #[arg(long)]
    no_p2p: bool,

    /// Use in-memory storage instead of RocksDB.
    #[arg(long)]
    in_memory: bool,

    /// Explorer API port. Set to 0 to disable.
    #[arg(long, default_value = "9955")]
    explorer_port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    info!("=== Solen Node v0.1.0 ===");
    info!(
        rpc_port = cli.rpc_port,
        p2p_port = cli.p2p_port,
        data_dir = %cli.data_dir,
        block_time_ms = cli.block_time,
        p2p = !cli.no_p2p,
    );

    // --- Storage backend ---
    let mut store: Box<dyn StateStore> = if cli.in_memory {
        info!("using in-memory storage (data will not persist)");
        Box::new(solen_storage::MemoryStore::new())
    } else {
        create_persistent_store(&cli.data_dir)?
    };

    // --- Validator key ---
    let validator_kp = match &cli.validator_seed {
        Some(hex) => {
            let bytes = hex_decode(hex)?;
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&bytes);
            Keypair::from_seed(&seed)
        }
        None => Keypair::from_seed(&[1u8; 32]),
    };
    let validator_id = validator_kp.public_key();

    // --- Genesis accounts ---
    let faucet_kp = Keypair::from_seed(&[42u8; 32]);
    let faucet_id = name_to_id(b"faucet");
    let alice_kp = Keypair::from_seed(&[10u8; 32]);
    let alice_id = name_to_id(b"alice");
    let bob_id = name_to_id(b"bob");

    if store.is_empty() {
        apply_genesis(
            store.as_mut(),
            vec![
                GenesisAccount {
                    id: faucet_id,
                    balance: 1_000_000_000,
                    auth_methods: vec![AuthMethod::Ed25519 {
                        public_key: faucet_kp.public_key(),
                    }],
                },
                GenesisAccount {
                    id: alice_id,
                    balance: 10_000,
                    auth_methods: vec![AuthMethod::Ed25519 {
                        public_key: alice_kp.public_key(),
                    }],
                },
                GenesisAccount {
                    id: bob_id,
                    balance: 5_000,
                    auth_methods: vec![],
                },
            ],
        )?;
        info!(
            faucet = hex(&faucet_id),
            alice = hex(&alice_id),
            bob = hex(&bob_id),
            "genesis state initialized"
        );
    } else {
        info!(state_root = hex(&store.state_root()), "loaded existing state");
    }

    // --- Consensus engine ---
    let config = EngineConfig {
        block_time_ms: cli.block_time,
        max_ops_per_block: 100,
        validator_id,
    };

    let mempool = Mempool::new(10_000);
    let engine = Arc::new(ConsensusEngine::new(config, store, mempool));

    // --- P2P networking ---
    let net_handle = if !cli.no_p2p {
        let bootstrap_peers: Vec<_> = cli
            .bootstrap
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();

        let net_config = NetworkConfig {
            listen_port: cli.p2p_port,
            bootstrap_peers,
        };

        let (handle, mut inbound_rx, _task) = NetworkService::start(net_config).await?;

        // Spawn a task to handle incoming P2P messages.
        let engine_for_p2p = engine.clone();
        tokio::spawn(async move {
            while let Some(msg) = inbound_rx.recv().await {
                match msg {
                    NetworkMessage::NewTransaction(op) => {
                        engine_for_p2p.mempool().submit(op);
                    }
                    NetworkMessage::NewBlock { header, .. } => {
                        tracing::debug!(
                            height = header.height,
                            proposer = hex(&header.proposer),
                            "received block from peer"
                        );
                    }
                    NetworkMessage::Attestation { .. } => {
                        // TODO: collect attestations for finality
                    }
                }
            }
        });

        Some(handle)
    } else {
        None
    };

    // --- RPC server ---
    let rpc_addr: SocketAddr = format!("127.0.0.1:{}", cli.rpc_port).parse()?;
    let _rpc_handle = start_rpc_server(rpc_addr, engine.clone()).await?;

    // --- Event indexer + Explorer API ---
    let index_store = std::sync::Arc::new(std::sync::RwLock::new(
        solen_indexer::store::IndexStore::new(),
    ));

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    {
        let engine_for_idx = engine.clone();
        let idx_store = index_store.clone();
        let idx_cancel = shutdown_rx.clone();
        tokio::spawn(async move {
            solen_indexer::indexer::run_indexer(engine_for_idx, idx_store, idx_cancel).await;
        });
    }

    if cli.explorer_port > 0 {
        let explorer_addr: SocketAddr =
            format!("127.0.0.1:{}", cli.explorer_port).parse()?;
        let explorer_store = index_store.clone();
        tokio::spawn(async move {
            if let Err(e) =
                solen_indexer::api::start_explorer_api(explorer_addr, explorer_store).await
            {
                tracing::error!(error = %e, "explorer API failed");
            }
        });
    }

    // --- Block production loop ---

    let engine_clone = engine.clone();
    let net_for_blocks = net_handle.clone();
    let consensus_handle = tokio::spawn(async move {
        let mut tick =
            tokio::time::interval(tokio::time::Duration::from_millis(cli.block_time));

        loop {
            tick.tick().await;

            if *shutdown_rx.borrow() {
                info!("consensus engine stopping");
                break;
            }

            let block = engine_clone.produce_block();

            // Broadcast new block over P2P.
            if let Some(ref handle) = net_for_blocks {
                handle.broadcast(NetworkMessage::NewBlock {
                    header: block.header.clone(),
                    tx_count: block.result.receipts.len(),
                    gas_used: block.result.gas_used,
                });
            }
        }
    });

    info!("Node running. Press Ctrl+C to stop.");

    tokio::signal::ctrl_c().await?;
    info!("Shutdown signal received");
    shutdown_tx.send(true)?;
    consensus_handle.await?;

    info!(height = engine.height(), "Node stopped");

    Ok(())
}

fn create_persistent_store(data_dir: &str) -> anyhow::Result<Box<dyn StateStore>> {
    #[cfg(feature = "rocksdb")]
    {
        let path = std::path::PathBuf::from(data_dir);
        std::fs::create_dir_all(&path)?;
        let store = solen_storage::RocksStore::open(&path)?;
        info!(path = %path.display(), "using RocksDB storage");
        return Ok(Box::new(store));
    }

    #[cfg(not(feature = "rocksdb"))]
    {
        let _ = data_dir;
        info!("RocksDB not compiled in, using in-memory storage");
        Ok(Box::new(solen_storage::MemoryStore::new()))
    }
}

fn name_to_id(name: &[u8]) -> [u8; 32] {
    let mut id = [0u8; 32];
    let len = name.len().min(32);
    id[..len].copy_from_slice(&name[..len]);
    id
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into))
        .collect()
}
