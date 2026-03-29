//! Solen node entrypoint.
//!
//! Wires together consensus, execution, networking, and RPC.

mod genesis_config;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use genesis_config::GenesisConfig;
use solen_consensus::engine::{ConsensusEngine, EngineConfig};
use solen_consensus::mempool::Mempool;
use solen_crypto::Keypair;
use solen_p2p::messages::NetworkMessage;
use solen_p2p::network::{NetworkConfig, NetworkService};
use solen_rpc::server::start_rpc_server;
use solen_storage::StateStore;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Network environment.
///
/// Port scheme:
///   mainnet: RPC 9944,  P2P 30333,  Explorer 9955
///   testnet: RPC 19944, P2P 40333,  Explorer 19955
///   devnet:  RPC 29944, P2P 50333,  Explorer 29955
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
enum Network {
    #[default]
    Devnet,
    Testnet,
    Mainnet,
}

impl Network {
    fn port_offset(self) -> u16 {
        match self {
            Network::Mainnet => 0,
            Network::Testnet => 10000,
            Network::Devnet => 20000,
        }
    }

    fn p2p_offset(self) -> u16 {
        match self {
            Network::Mainnet => 0,
            Network::Testnet => 10000,
            Network::Devnet => 20000,
        }
    }

    fn default_data_dir(self) -> &'static str {
        match self {
            Network::Devnet => "data/devnet",
            Network::Testnet => "data/testnet",
            Network::Mainnet => "data/mainnet",
        }
    }

    #[allow(dead_code)]
    fn default_block_time(self) -> u64 {
        match self {
            Network::Devnet => 2000,
            Network::Testnet => 2000,
            Network::Mainnet => 6000,
        }
    }
}

/// Solen blockchain node.
#[derive(Parser)]
#[command(name = "solen-node", version = "0.1.0")]
struct Cli {
    /// Network environment (devnet, testnet, mainnet).
    /// Sets default ports, data directory, and block time.
    #[arg(long, default_value = "devnet")]
    network: Network,

    /// RPC server listen port. Defaults: mainnet=9944, testnet=19944, devnet=29944.
    #[arg(long)]
    rpc_port: Option<u16>,

    /// P2P listen port. Defaults: mainnet=30333, testnet=40333, devnet=50333.
    #[arg(long)]
    p2p_port: Option<u16>,

    /// Data directory for persistent storage.
    #[arg(long)]
    data_dir: Option<String>,

    /// Block production interval in milliseconds.
    #[arg(long)]
    block_time: Option<u64>,

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

    /// Explorer API port. Set to 0 to disable. Defaults: mainnet=9955, testnet=19955, devnet=29955.
    #[arg(long)]
    explorer_port: Option<u16>,

    /// Path to genesis.json config file. If not set, uses default config for the network.
    #[arg(long)]
    genesis: Option<String>,

    /// Generate a genesis.json file for the selected network and exit.
    #[arg(long)]
    init_genesis: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Resolve defaults from network.
    let net = cli.network;
    let rpc_port = cli.rpc_port.unwrap_or(9944 + net.port_offset());
    let p2p_port = cli.p2p_port.unwrap_or(30333 + net.p2p_offset());
    let explorer_port = cli.explorer_port.unwrap_or(9955 + net.port_offset());
    let data_dir = cli
        .data_dir
        .clone()
        .unwrap_or_else(|| net.default_data_dir().to_string());

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    info!("=== Solen Node v0.1.0 ===");

    // --- Load genesis config ---
    let genesis = if let Some(path) = &cli.genesis {
        GenesisConfig::load(&PathBuf::from(path))?
    } else {
        match net {
            Network::Devnet => GenesisConfig::default_devnet(),
            Network::Testnet => GenesisConfig::default_testnet(),
            Network::Mainnet => GenesisConfig::default_testnet(), // TODO: mainnet config
        }
    };

    // Handle --init-genesis: write config to file and exit.
    if cli.init_genesis {
        let out_path = PathBuf::from(&data_dir).join("genesis.json");
        genesis.save(&out_path)?;
        info!(path = %out_path.display(), "genesis config written");
        return Ok(());
    }

    let block_time = cli.block_time.unwrap_or(genesis.block_time_ms);

    info!(
        network = ?net,
        chain = %genesis.chain_name,
        chain_id = genesis.chain_id,
        rpc_port,
        p2p_port,
        explorer_port,
        data_dir = %data_dir,
        block_time_ms = block_time,
        p2p = !cli.no_p2p,
    );

    // --- Storage backend ---
    let mut store: Box<dyn StateStore> = if cli.in_memory {
        info!("using in-memory storage (data will not persist)");
        Box::new(solen_storage::MemoryStore::new())
    } else {
        create_persistent_store(&data_dir)?
    };

    // --- Validator key ---
    let validator_kp = if let Some(hex) = &cli.validator_seed {
        let bytes = hex_decode(hex)?;
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        Keypair::from_seed(&seed)
    } else if let Some(v) = genesis.validators.first() {
        let seed = hex_decode(&v.seed_hex)?;
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&seed);
        Keypair::from_seed(&arr)
    } else {
        Keypair::from_seed(&[1u8; 32])
    };
    let validator_id = validator_kp.public_key();

    // --- Apply genesis if store is empty ---
    if store.is_empty() {
        genesis.apply(store.as_mut())?;
    } else {
        info!(state_root = hex(&store.state_root()), "loaded existing state");
    }

    // --- Consensus engine ---
    let config = EngineConfig {
        block_time_ms: block_time,
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
            listen_port: p2p_port,
            bootstrap_peers,
        };

        let (handle, mut inbound_rx, _task) = NetworkService::start(net_config).await?;

        // Spawn a task to handle incoming P2P messages.
        let engine_for_p2p = engine.clone();
        let net_for_attest = handle.clone();
        tokio::spawn(async move {
            while let Some(msg) = inbound_rx.recv().await {
                match msg {
                    NetworkMessage::NewTransaction(op) => {
                        engine_for_p2p.mempool().submit(op);
                    }
                    NetworkMessage::NewBlock {
                        header,
                        operations,
                        ..
                    } => {
                        // Validate and accept the block.
                        if engine_for_p2p.accept_block(&header, &operations) {
                            // Send our attestation back.
                            let bh = solen_consensus::engine::block_hash(&header);
                            let att_msg = NetworkMessage::Attestation {
                                validator_id: engine_for_p2p.validator_id(),
                                block_height: header.height,
                                block_hash: bh,
                                signature: vec![], // TODO: sign attestation
                            };
                            net_for_attest.broadcast(att_msg);

                            // Also self-attest locally.
                            engine_for_p2p.accept_attestation(
                                engine_for_p2p.validator_id(),
                                header.height,
                                bh,
                            );
                        }
                    }
                    NetworkMessage::Attestation {
                        validator_id,
                        block_height,
                        block_hash,
                        ..
                    } => {
                        engine_for_p2p.accept_attestation(
                            validator_id,
                            block_height,
                            block_hash,
                        );
                    }
                }
            }
        });

        Some(handle)
    } else {
        None
    };

    // --- RPC server ---
    let rpc_addr: SocketAddr = format!("127.0.0.1:{}", rpc_port).parse()?;
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

    if explorer_port > 0 {
        let explorer_addr: SocketAddr =
            format!("127.0.0.1:{}", explorer_port).parse()?;
        let explorer_store = index_store.clone();
        let explorer_engine = engine.clone();
        tokio::spawn(async move {
            if let Err(e) =
                solen_indexer::api::start_explorer_api(explorer_addr, explorer_store, Some(explorer_engine)).await
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
            tokio::time::interval(tokio::time::Duration::from_millis(block_time));

        loop {
            tick.tick().await;

            if *shutdown_rx.borrow() {
                info!("consensus engine stopping");
                break;
            }

            // Only produce if it's our turn (or single-validator mode).
            if engine_clone.active_validator_count() <= 1 || engine_clone.is_next_proposer() {
                let produced = engine_clone.produce_block();

                // Broadcast the proposed block with full operations.
                if let Some(ref handle) = net_for_blocks {
                    let gas = produced.finalized.as_ref().map(|b| b.result.gas_used).unwrap_or(0);
                    let tx_count = produced.operations.len();
                    handle.broadcast(NetworkMessage::NewBlock {
                        header: produced.header,
                        operations: produced.operations,
                        tx_count,
                        gas_used: gas,
                    });
                }
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
