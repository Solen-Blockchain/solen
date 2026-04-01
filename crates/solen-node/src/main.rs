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

    /// Archive mode: disable block pruning, keep all history.
    #[arg(long)]
    archive: bool,

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
    let (validator_kp, validator_seed) = if let Some(hex) = &cli.validator_seed {
        let bytes = hex_decode(hex)?;
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        (Keypair::from_seed(&seed), seed)
    } else if let Some(v) = genesis.validators.first() {
        if let Some(seed_hex) = &v.seed_hex {
            let seed = hex_decode(seed_hex)?;
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&seed);
            (Keypair::from_seed(&arr), arr)
        } else {
            anyhow::bail!(
                "no --validator-seed provided and genesis validator '{}' has no seed_hex. \
                 On mainnet, you must provide --validator-seed.",
                v.name
            );
        }
    } else {
        anyhow::bail!(
            "no validators in genesis config and no --validator-seed provided. \
             Cannot start without a validator identity."
        );
    };
    let validator_id = validator_kp.public_key();

    // --- Apply genesis if store is empty ---
    if store.is_empty() {
        genesis.apply(store.as_mut())?;
    } else {
        info!(state_root = hex(&store.state_root()), "loaded existing state");
    }

    // --- Consensus engine ---
    // Build validator set from genesis config using public keys.
    let validator_set = {
        use solen_consensus::validator::{ValidatorInfo, ValidatorSet};
        let mut validators = Vec::new();
        for v in &genesis.validators {
            let public_key = if let Some(seed_hex) = &v.seed_hex {
                let seed = hex_decode(seed_hex)?;
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&seed);
                Keypair::from_seed(&arr).public_key()
            } else if let Some(pk_hex) = &v.public_key_hex {
                let bytes = hex_decode(pk_hex)?;
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                arr
            } else {
                anyhow::bail!("validator '{}' needs seed_hex or public_key_hex", v.name);
            };
            validators.push(ValidatorInfo::new(public_key, v.stake));
        }
        ValidatorSet::new(validators)
    };

    info!(
        validator_id = hex(&validator_id),
        validators = validator_set.active_count(),
        "validator set initialized from genesis"
    );

    let config = EngineConfig {
        block_time_ms: block_time,
        max_ops_per_block: 100,
        validator_id,
        chain_id: genesis.chain_id,
        archive: cli.archive,
    };

    let mempool = Mempool::new(10_000);
    let engine = Arc::new(ConsensusEngine::with_validators(config, store, mempool, validator_set));

    // Syncing flag: start in sync mode for multi-validator networks to prevent
    // producing blocks before we've caught up with the network.
    let is_multi = engine.active_validator_count() > 1;
    let syncing = Arc::new(std::sync::atomic::AtomicBool::new(is_multi));

    // Track the highest known network height (from StatusAnnounce messages).
    let network_height = Arc::new(std::sync::atomic::AtomicU64::new(0));

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
            identity_seed: Some(validator_seed),
            chain_id: genesis.chain_id,
            ..Default::default()
        };

        let (handle, mut inbound_rx, _task) = NetworkService::start(net_config).await?;

        // Spawn a task to handle incoming P2P messages.
        let engine_for_p2p = engine.clone();
        let net_for_attest = handle.clone();
        let syncing_for_p2p = syncing.clone();
        let net_height_for_p2p = network_height.clone();
        let peer_heights_for_p2p = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let peer_heights_for_status = peer_heights_for_p2p.clone();
        let attestation_kp = Arc::new(validator_kp);
        let att_kp_for_p2p = attestation_kp.clone();
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
                        // Reject oversized blocks to prevent memory DoS.
                        if operations.len() > 1000 {
                            tracing::warn!(ops = operations.len(), "rejecting oversized block");
                            continue;
                        }
                        // While syncing, still accept live blocks — the node needs
                        // to catch up even if sync protocol isn't delivering.
                        // accept_block handles fast-forward for gaps.
                        // Validate and accept the block.
                        if engine_for_p2p.accept_block(&header, &operations) {
                            // Block accepted with matching state root.
                            // If we were syncing, we're now verified in sync.
                            if syncing_for_p2p.swap(false, std::sync::atomic::Ordering::Relaxed) {
                                // Clear stale pending blocks/attestations from before sync.
                                engine_for_p2p.clear_stale_pending(header.height);
                                tracing::info!(
                                    height = header.height,
                                    "state verified — resuming block production"
                                );
                            }
                            // Send our signed attestation back.
                            let bh = solen_consensus::engine::block_hash(&header);
                            let att_payload = attestation_payload(header.height, &bh);
                            let att_sig = att_kp_for_p2p.sign(&att_payload);
                            let att_msg = NetworkMessage::Attestation {
                                validator_id: engine_for_p2p.validator_id(),
                                block_height: header.height,
                                block_hash: bh,
                                signature: att_sig.to_vec(),
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
                        signature,
                    } => {
                        // Verify attestation signature.
                        let payload = attestation_payload(block_height, &block_hash);
                        if signature.len() == 64 {
                            let mut sig = [0u8; 64];
                            sig.copy_from_slice(&signature);
                            if solen_crypto::verify(&validator_id, &payload, &sig).is_ok() {
                                engine_for_p2p.accept_attestation(
                                    validator_id,
                                    block_height,
                                    block_hash,
                                );
                            } else {
                                let v_hex = hex(&validator_id);
                                tracing::warn!(
                                    validator = &v_hex[..8],
                                    height = block_height,
                                    "invalid attestation signature — rejected"
                                );
                            }
                        } else {
                            tracing::warn!(
                                height = block_height,
                                "attestation with invalid signature length — rejected"
                            );
                        }
                    }
                    NetworkMessage::StatusAnnounce { height, .. } => {
                        // Track peer heights to prevent a single rogue node from
                        // stalling the network with a fake longer chain.
                        peer_heights_for_p2p.lock().unwrap().push(height);

                        let our_height = engine_for_p2p.height();
                        if height > our_height + 1 {
                            // Check if multiple peers agree we're behind before
                            // entering sync mode. A single rogue peer can't stall us.
                            let peer_h = peer_heights_for_p2p.lock().unwrap();
                            let peers_ahead = peer_h.iter().filter(|&&h| h > our_height + 1).count();
                            let total_peers = peer_h.len();
                            drop(peer_h);

                            // Need at least 2 peers ahead, or if only 1 peer total, trust it.
                            if peers_ahead >= 2 || total_peers <= 1 {
                                net_height_for_p2p.fetch_max(height, std::sync::atomic::Ordering::Relaxed);
                                syncing_for_p2p.store(true, std::sync::atomic::Ordering::Relaxed);
                                tracing::info!(
                                    our_height,
                                    peer_height = height,
                                    peers_ahead,
                                    "peers confirm we are behind, requesting sync"
                                );
                                net_for_attest.broadcast(NetworkMessage::SyncRequest {
                                    from_height: our_height + 1,
                                    to_height: height,
                                });
                            } else {
                                tracing::debug!(
                                    our_height,
                                    peer_height = height,
                                    peers_ahead,
                                    "single peer claims higher height — waiting for confirmation"
                                );
                            }
                        } else {
                            net_height_for_p2p.fetch_max(height, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    NetworkMessage::SyncRequest { from_height, to_height } => {
                        // Serve blocks to the requesting peer.
                        let max_batch = 20;
                        let _to = to_height.min(from_height + max_batch as u64 - 1);
                        let blocks = engine_for_p2p.get_blocks_for_sync(from_height, max_batch);

                        if !blocks.is_empty() {
                            let sync_blocks: Vec<solen_p2p::messages::SyncBlock> = blocks
                                .iter()
                                .map(|b| solen_p2p::messages::SyncBlock {
                                    header: b.header.clone(),
                                    operations: b.operations.clone(),
                                    receipts: b.result.receipts.clone(),
                                })
                                .collect();

                            tracing::info!(
                                from = from_height,
                                count = sync_blocks.len(),
                                "serving sync blocks to peer"
                            );

                            net_for_attest.broadcast(NetworkMessage::SyncBlocks {
                                blocks: sync_blocks,
                            });
                        }
                    }
                    NetworkMessage::SyncBlocks { mut blocks } => {
                        if blocks.is_empty() {
                            continue;
                        }
                        // Cap sync batch size to prevent memory DoS.
                        if blocks.len() > 100 {
                            blocks.truncate(100);
                        }

                        // Sort by height so out-of-order arrivals are processed correctly.
                        blocks.sort_by_key(|b| b.header.height);

                        let mut synced = 0u64;
                        let mut highest_peer_height = 0u64;

                        for sync_block in &blocks {
                            if sync_block.header.height > highest_peer_height {
                                highest_peer_height = sync_block.header.height;
                            }
                            if sync_block.header.height <= engine_for_p2p.height() {
                                continue; // already have this block
                            }
                            if sync_block.header.height != engine_for_p2p.height() + 1 {
                                continue; // gap — can't apply yet
                            }
                            engine_for_p2p.replay_synced_block(
                                &sync_block.header,
                                &sync_block.operations,
                                sync_block.receipts.clone(),
                            );
                            synced += 1;
                        }

                        if synced > 0 {
                            let our_height = engine_for_p2p.height();
                            let known_net_height = net_height_for_p2p.load(std::sync::atomic::Ordering::Relaxed);
                            tracing::info!(
                                synced,
                                new_height = our_height,
                                network_height = known_net_height,
                                "synced blocks from peer"
                            );

                            // Check if we've caught up to the known network height.
                            if our_height + 1 >= known_net_height {
                                // Don't resume production yet — wait for a live block
                                // to verify our state root matches the network.
                                // The syncing flag stays true; it'll be cleared when
                                // we successfully accept a live block (state root matches).
                                tracing::info!(
                                    height = our_height,
                                    "sync caught up, waiting to verify state with live block"
                                );
                            } else {
                                // Still behind — request more blocks.
                                net_for_attest.broadcast(NetworkMessage::SyncRequest {
                                    from_height: our_height + 1,
                                    to_height: known_net_height,
                                });
                            }
                        }
                    }
                }
            }
        });

        // Periodically broadcast our height so new nodes can request sync.
        let status_engine = engine.clone();
        let status_handle = handle.clone();
        let syncing_for_status = syncing.clone();
        let net_height_for_status = network_height.clone();
        tokio::spawn(async move {
            // Broadcast immediately after a short delay for mesh warmup.
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(15));
            let mut ticks_since_start = 0u32;

            loop {
                let height = status_engine.height();
                let state_root = status_engine.store().read().unwrap().state_root();
                status_handle.broadcast(NetworkMessage::StatusAnnounce {
                    height,
                    state_root,
                });

                // If still in sync mode, check if we can resume production.
                ticks_since_start += 1;
                if syncing_for_status.load(std::sync::atomic::Ordering::Relaxed) {
                    let known = net_height_for_status.load(std::sync::atomic::Ordering::Relaxed);

                    let has_peers = !peer_heights_for_status.lock().unwrap().is_empty();
                    if height == 0 && !has_peers && ticks_since_start >= 5 {
                        // Genesis: no peers connected after 75s, start solo.
                        tracing::info!("no peers found — starting as first validator");
                        syncing_for_status.store(false, std::sync::atomic::Ordering::Relaxed);
                    } else if height == 0 && has_peers && ticks_since_start >= 5 {
                        // Genesis with peers: wait long enough for gossipsub mesh
                        // to form and the first proposer's block to arrive.
                        tracing::info!("genesis timeout with peers — resuming block production");
                        syncing_for_status.store(false, std::sync::atomic::Ordering::Relaxed);
                    } else if known > 0 && height + 1 >= known && ticks_since_start >= 4 {
                        // We've heard from a peer AND we're at their height,
                        // but haven't accepted a live block yet.
                        // Safety fallback after 60s.
                        tracing::info!(height, network_height = known, "timeout — resuming block production");
                        syncing_for_status.store(false, std::sync::atomic::Ordering::Relaxed);
                    }
                    // Otherwise: keep waiting for a live block to verify state.
                }

                interval.tick().await;
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
    let syncing_for_consensus = syncing.clone();
    let consensus_handle = tokio::spawn(async move {
        // Wait for P2P mesh to form before producing blocks.
        // Gossipsub needs several heartbeats to build the mesh after peers connect.
        if engine_clone.active_validator_count() > 1 {
            info!("waiting 10s for P2P mesh to form before block production...");
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            info!("starting block production");
        }

        // Poll frequently but enforce block_time between proposals.
        let mut poll = tokio::time::interval(tokio::time::Duration::from_millis(200));
        let mut min_interval = std::time::Duration::from_millis(block_time);
        let quorum_timeout = std::time::Duration::from_secs(10);
        let mut last_finalized_height = engine_clone.height();
        let mut last_finalized_at = std::time::Instant::now();

        loop {
            poll.tick().await;

            if *shutdown_rx.borrow() {
                info!("consensus engine stopping");
                break;
            }

            // Don't do anything consensus-related while syncing.
            if syncing_for_consensus.load(std::sync::atomic::Ordering::Relaxed) {
                // Still track finalization from sync so we don't stall after sync completes.
                let current_height = engine_clone.height();
                if current_height > last_finalized_height {
                    last_finalized_height = current_height;
                    last_finalized_at = std::time::Instant::now();
                }
                continue;
            }

            // Force-finalize blocks stuck waiting for quorum.
            let force_finalized = engine_clone.finalize_timed_out_blocks(quorum_timeout);

            // If we force-finalized, broadcast status so peers can sync.
            if force_finalized > 0 {
                if let Some(ref handle) = net_for_blocks {
                    let height = engine_clone.height();
                    let state_root = engine_clone.store().read().unwrap().state_root();
                    handle.broadcast(NetworkMessage::StatusAnnounce { height, state_root });
                }
            }

            // Track when new blocks finalize (from any source).
            let current_height = engine_clone.height();
            if current_height > last_finalized_height {
                last_finalized_height = current_height;
                last_finalized_at = std::time::Instant::now();

                // Check if block time was changed by governance.
                if current_height % 100 == 0 {
                    let store_lock = engine_clone.store();
                    let store = store_lock.read().unwrap();
                    if let Ok(Some(data)) = store.get(b"__config_block_time__") {
                        if data.len() >= 8 {
                            let mut buf = [0u8; 8];
                            buf.copy_from_slice(&data[..8]);
                            let new_bt = u64::from_le_bytes(buf);
                            if new_bt > 0 && new_bt != min_interval.as_millis() as u64 {
                                info!(old_ms = min_interval.as_millis(), new_ms = new_bt, "block time updated by governance");
                                min_interval = std::time::Duration::from_millis(new_bt);
                            }
                        }
                    }
                }
            }

            // Enforce minimum interval since last finalized block.
            if last_finalized_at.elapsed() < min_interval {
                continue;
            }

            // Produce if it's our turn, or take over if the proposer is offline.
            let next_height = engine_clone.height() + 1;
            let already_pending = engine_clone.has_pending_block(next_height);
            let stalled_for = last_finalized_at.elapsed();

            let should_propose = engine_clone.active_validator_count() <= 1
                || (engine_clone.is_next_proposer() && !already_pending)
                || (!already_pending && engine_clone.is_backup_proposer(stalled_for));

            if should_propose {
                let produced = engine_clone.produce_block();
                last_finalized_at = std::time::Instant::now();

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

/// Build the deterministic payload for attestation signing/verification.
fn attestation_payload(height: u64, block_hash: &[u8; 32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(40);
    payload.extend_from_slice(&height.to_le_bytes());
    payload.extend_from_slice(block_hash);
    payload
}

fn hex_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into))
        .collect()
}
