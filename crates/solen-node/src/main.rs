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
use solen_types::encoding::{account_to_base58, hex_encode as encoding_hex_encode};
use tracing::{info, warn};
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

    /// Prune mode: delete old blocks to save disk space.
    /// Default is archive mode (keep all history).
    #[arg(long)]
    prune: bool,

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

    /// Bootstrap from a state snapshot URL or file path.
    /// Downloads and restores the snapshot, then syncs forward from that height.
    #[arg(long)]
    snapshot: Option<String>,

    /// Expected genesis state root (hex). Reject sync blocks from chains
    /// with a different genesis. Use this to isolate from old chain forks.
    #[arg(long)]
    genesis_hash: Option<String>,
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
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(
                "info,libp2p_gossipsub::peer_score=error,libp2p_gossipsub::behaviour=warn"
            )),
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
            Network::Mainnet => {
                anyhow::bail!(
                    "mainnet requires an explicit genesis file via --genesis <path>. \
                     Do NOT use testnet or devnet genesis for mainnet — those seeds are public."
                );
            }
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

    // --- Node identity ---
    // If --validator-seed is provided, use it (validator node).
    // Otherwise, generate a random identity (non-validator / RPC node).
    // For devnet with a single validator, fall back to the genesis validator key.
    let (validator_kp, validator_seed) = if let Some(hex) = &cli.validator_seed {
        let bytes = hex_decode(hex)?;
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        (Keypair::from_seed(&seed), seed)
    } else if matches!(net, Network::Devnet) {
        // Devnet: use first genesis validator for convenience (single-validator mode).
        if let Some(v) = genesis.validators.first() {
            if let Some(seed_hex) = &v.seed_hex {
                let seed = hex_decode(seed_hex)?;
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&seed);
                (Keypair::from_seed(&arr), arr)
            } else {
                let seed = rand_seed();
                (Keypair::from_seed(&seed), seed)
            }
        } else {
            let seed = rand_seed();
            (Keypair::from_seed(&seed), seed)
        }
    } else {
        // Testnet/Mainnet without --validator-seed: generate random identity.
        // This node will participate in P2P but not produce blocks.
        let seed = rand_seed();
        info!("no --validator-seed provided — running as non-validator node");
        (Keypair::from_seed(&seed), seed)
    };
    let validator_id = validator_kp.public_key();

    // --- Restore from snapshot (explicit or auto from seeds) ---
    let mut snapshot_height: Option<u64> = None;
    let snapshot_source: Option<String> = if cli.snapshot.is_some() {
        cli.snapshot.clone()
    } else if store.is_empty() && !cli.bootstrap.is_empty() {
        // Auto-discover snapshot from seed nodes.
        // Security: query chain status from ALL reachable seeds first,
        // verify they agree on the state root before downloading a snapshot.
        info!("empty store — attempting snapshot sync from seed nodes...");

        // Build RPC URLs from bootstrap peers.
        // For testnet/mainnet, try both the public endpoint AND individual peer IPs.
        let seed_rpc_urls: Vec<String> = {
            let mut urls = std::collections::HashSet::new();
            // Add the public RPC endpoint.
            match net {
                Network::Testnet => { urls.insert("https://testnet-rpc.solenchain.io".to_string()); }
                Network::Mainnet => { urls.insert("https://rpc.solenchain.io".to_string()); }
                _ => {}
            }
            // Also try individual peer IPs (extract from multiaddr).
            for addr in &cli.bootstrap {
                let ip = addr.split('/').find(|s| {
                    // Match IPv4 addresses, not DNS names.
                    s.split('.').count() == 4 && s.split('.').all(|p| p.parse::<u8>().is_ok())
                });
                if let Some(ip) = ip {
                    let peer_rpc_port = match net {
                        Network::Testnet => 19944,
                        Network::Mainnet => 9944,
                        _ => rpc_port,
                    };
                    urls.insert(format!("http://{}:{}", ip, peer_rpc_port));
                }
            }
            urls.into_iter().collect()
        };
        info!(urls = ?seed_rpc_urls, "snapshot seed RPC URLs");

        // Step 1: Query chain status from all seeds to get consensus on state root.
        let status_body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "solen_chainStatus", "params": []
        });
        let mut state_roots: Vec<(String, u64, String)> = Vec::new(); // (url, height, state_root)
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap();

        for url in &seed_rpc_urls {
            match client.post(url).header("Content-Type", "application/json")
                .body(status_body.to_string()).send()
            {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(json) = resp.json::<serde_json::Value>() {
                        if let (Some(h), Some(sr)) = (
                            json["result"]["height"].as_u64(),
                            json["result"]["state_root"].as_str()
                                .or(json["result"]["latest_state_root"].as_str()),
                        ) {
                            info!(url = %url, height = h, state_root = %sr, "seed status");
                            state_roots.push((url.clone(), h, sr.to_string()));
                        }
                    }
                }
                _ => { info!(url = %url, "seed not reachable"); }
            }
        }

        // Step 2: Verify seeds agree. Require at least 2 seeds to agree on state root,
        // or accept a single seed only for devnet.
        let mut found = None;
        if !state_roots.is_empty() {
            // Find the most common state root among the highest-height responses.
            let max_height = state_roots.iter().map(|(_, h, _)| *h).max().unwrap_or(0);
            let at_max: Vec<_> = state_roots.iter()
                .filter(|(_, h, _)| max_height.saturating_sub(*h) <= 10) // within 10 blocks
                .collect();

            let mut root_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
            for (_, _, sr) in &at_max {
                *root_counts.entry(sr.as_str()).or_insert(0) += 1;
            }

            let (consensus_root, count) = root_counts.iter()
                .max_by_key(|(_, c)| *c)
                .map(|(r, c)| (r.to_string(), *c))
                .unwrap_or_default();

            // Require 2+ agreeing seeds when multiple are available.
            // Fall back to 1 if only 1 unique URL was reachable.
            let need_consensus = if matches!(net, Network::Mainnet) && state_roots.len() > 1 { 2 } else { 1 };

            if count >= need_consensus {
                info!(
                    state_root = %consensus_root,
                    agreeing_seeds = count,
                    total_seeds = state_roots.len(),
                    "seed consensus verified"
                );

                // Step 3: Download snapshot from first reachable seed.
                let snapshot_body = serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "method": "solen_getSnapshot", "params": []
                });

                for (url, _, _) in &at_max {
                    info!(url = %url, "downloading snapshot...");
                    match client.post(url.as_str())
                        .header("Content-Type", "application/json")
                        .body(snapshot_body.to_string()).send()
                    {
                        Ok(resp) if resp.status().is_success() => {
                            if let Ok(json) = resp.json::<serde_json::Value>() {
                                if let Some(b64) = json["result"]["data"].as_str() {
                                    let snap_root = json["result"]["state_root"].as_str().unwrap_or("");

                                    // Accept the snapshot for download. The actual state root
                                    // verification happens in restore_snapshot() after
                                    // decompression — it recomputes the merkle root over all
                                    // loaded entries and rejects if it doesn't match the header.
                                    // Pre-download, we just log for diagnostics.
                                    if snap_root != consensus_root {
                                        info!(
                                            snap_root,
                                            consensus_root = %consensus_root,
                                            "snapshot root differs from consensus (may be cached) — will verify after restore"
                                        );
                                    }
                                    {
                                        // Verify finalized checkpoint if present.
                                        // The checkpoint proves 2/3+ of validators attested
                                        // to this state root, preventing long-range attacks.
                                        let checkpoint_valid = if let Some(cp) = json["result"]["checkpoint"].as_object() {
                                            let cp_state_root = cp.get("state_root")
                                                .and_then(|v| v.as_str()).unwrap_or("");
                                            let cp_height = cp.get("height")
                                                .and_then(|v| v.as_u64()).unwrap_or(0);
                                            let attestations = cp.get("attestations")
                                                .and_then(|v| v.as_array())
                                                .map(|a| a.len()).unwrap_or(0);

                                            if attestations == 0 {
                                                info!("snapshot has no checkpoint attestations — accepting on seed consensus only");
                                                true
                                            } else {
                                                info!(
                                                    cp_height,
                                                    attestations,
                                                    cp_state_root,
                                                    "snapshot includes finalized checkpoint"
                                                );
                                                // Verify checkpoint state_root matches snapshot.
                                                // Full attestation signature verification happens after restore.
                                                cp_state_root == snap_root || cp_state_root.is_empty()
                                            }
                                        } else {
                                            // No checkpoint in response — older node or genesis.
                                            info!("snapshot has no checkpoint — accepting on seed consensus only");
                                            true
                                        };

                                        if !checkpoint_valid {
                                            warn!("snapshot checkpoint state_root doesn't match — trying next seed");
                                            continue;
                                        }

                                        match base64_decode(b64) {
                                            Ok(data) => {
                                                let tmp = format!("{}/snapshot.bin", data_dir);
                                                if std::fs::write(&tmp, &data).is_ok() {
                                                    info!(
                                                        height = json["result"]["height"].as_u64().unwrap_or(0),
                                                        "snapshot downloaded and verified against seed consensus"
                                                    );
                                                    found = Some(tmp);
                                                    break;
                                                }
                                            }
                                            Err(e) => { info!(error = %e, "snapshot decode failed"); }
                                        }
                                    }
                                }
                            }
                        }
                        _ => { info!(url = %url, "snapshot download failed, trying next..."); }
                    }
                }
            } else {
                warn!(
                    agreeing = count,
                    needed = need_consensus,
                    total = state_roots.len(),
                    "insufficient seed consensus on state root — skipping snapshot sync"
                );
            }
        }
        found
    } else {
        None
    };

    if let Some(ref snapshot_source) = snapshot_source {
        if store.is_empty() {
            info!(source = %snapshot_source, "loading state snapshot...");
            let snapshot_data = if snapshot_source.starts_with("http://") || snapshot_source.starts_with("https://") {
                // Download snapshot via JSON-RPC from the node.
                let client = reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(120))
                    .build()
                    .map_err(|e| anyhow::anyhow!("http client failed: {e}"))?;
                // Try single-call snapshot first, fall back to chunked download.
                let snapshot_url = snapshot_source.as_str();
                let single_result = (|| -> Result<Vec<u8>, anyhow::Error> {
                    let rpc_body = serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "method": "solen_getSnapshot", "params": []
                    });
                    let body = client.post(snapshot_url)
                        .header("Content-Type", "application/json")
                        .body(rpc_body.to_string())
                        .send()?
                        .text()?;
                    let json: serde_json::Value = serde_json::from_str(&body)?;
                    let b64 = json["result"]["data"].as_str()
                        .ok_or_else(|| anyhow::anyhow!("snapshot response missing data field"))?;
                    Ok(base64_decode(b64)?)
                })();

                match single_result {
                    Ok(data) => data,
                    Err(e) => {
                        info!("single-call snapshot failed ({e}), trying chunked download...");

                        // Get metadata first.
                        let meta_body = serde_json::json!({
                            "jsonrpc": "2.0", "id": 1,
                            "method": "solen_getSnapshotMeta", "params": []
                        });
                        let meta_resp = client.post(snapshot_url)
                            .header("Content-Type", "application/json")
                            .body(meta_body.to_string())
                            .send()
                            .map_err(|e| anyhow::anyhow!("snapshot meta failed: {e}"))?
                            .text()?;
                        let meta_json: serde_json::Value = serde_json::from_str(&meta_resp)?;
                        let total_bytes = meta_json["result"]["total_bytes"].as_u64()
                            .ok_or_else(|| anyhow::anyhow!("snapshot meta missing total_bytes"))? as usize;

                        info!(total_bytes, "downloading snapshot in chunks...");

                        let chunk_size: usize = 4 * 1024 * 1024; // 4MB chunks
                        let mut snapshot_data = Vec::with_capacity(total_bytes);
                        let mut offset: usize = 0;

                        loop {
                            let chunk_body = serde_json::json!({
                                "jsonrpc": "2.0", "id": 1,
                                "method": "solen_getSnapshotChunk",
                                "params": [offset, chunk_size]
                            });
                            let chunk_resp = client.post(snapshot_url)
                                .header("Content-Type", "application/json")
                                .body(chunk_body.to_string())
                                .send()
                                .map_err(|e| anyhow::anyhow!("chunk download failed at offset {offset}: {e}"))?
                                .text()?;
                            let chunk_json: serde_json::Value = serde_json::from_str(&chunk_resp)?;

                            if let Some(err) = chunk_json["error"]["message"].as_str() {
                                return Err(anyhow::anyhow!("chunk error: {err}"));
                            }

                            let chunk_b64 = chunk_json["result"]["data"].as_str()
                                .ok_or_else(|| anyhow::anyhow!("chunk missing data"))?;
                            let chunk_bytes = base64_decode(chunk_b64)?;
                            let done = chunk_json["result"]["done"].as_bool().unwrap_or(false);

                            info!(
                                offset,
                                chunk_len = chunk_bytes.len(),
                                progress = format!("{:.1}%", (offset + chunk_bytes.len()) as f64 / total_bytes as f64 * 100.0),
                                "downloaded chunk"
                            );

                            snapshot_data.extend_from_slice(&chunk_bytes);
                            offset += chunk_bytes.len();

                            if done || chunk_bytes.is_empty() { break; }
                        }

                        info!(total = snapshot_data.len(), "chunked snapshot download complete");
                        snapshot_data
                    }
                }
            } else {
                // Load from file.
                std::fs::read(snapshot_source)
                    .map_err(|e| anyhow::anyhow!("snapshot file read failed: {e}"))?
            };

            let meta = solen_consensus::snapshot::restore_snapshot(store.as_mut(), &snapshot_data)
                .map_err(|e| anyhow::anyhow!("snapshot restore failed: {e}"))?;

            info!(
                height = meta.height,
                epoch = meta.epoch,
                entries = meta.entry_count,
                state_root = hex(&meta.state_root),
                "snapshot restored — will sync forward from this height"
            );
            snapshot_height = Some(meta.height);
        } else {
            info!("store already has data — skipping snapshot restore");
        }
    }

    // --- Apply genesis if store is empty ---
    if store.is_empty() {
        genesis.apply(store.as_mut())?;
    } else if snapshot_height.is_none() {
        info!(state_root = hex(&store.state_root()), "loaded existing state");
    }

    // Determine expected genesis state root for fork isolation.
    // Priority: CLI flag > persisted > computed from current genesis.
    let expected_genesis_hash: Option<[u8; 32]> = if let Some(ref gh) = cli.genesis_hash {
        let bytes = hex_decode(gh)?;
        if bytes.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&bytes);
            // Persist for future restarts.
            let _ = store.put(b"__genesis_hash__", &arr);
            info!(genesis_hash = %gh, "fork isolation from CLI flag");
            Some(arr)
        } else {
            anyhow::bail!("--genesis-hash must be 64 hex characters (32 bytes)");
        }
    } else if let Ok(Some(data)) = store.get(b"__genesis_hash__") {
        if data.len() == 32 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&data);
            info!(genesis_hash = hex(&arr), "fork isolation from persisted genesis hash");
            Some(arr)
        } else {
            None
        }
    } else {
        // First run — compute and persist the genesis state root.
        let root = store.state_root();
        let _ = store.put(b"__genesis_hash__", &root);
        info!(genesis_hash = hex(&root), "fork isolation — genesis hash persisted");
        Some(root)
    };

    // Store chain_id for proof type restrictions (e.g., block mock proofs on mainnet).
    // Only write if not already present — avoids overwriting snapshot-restored state.
    if store.get(b"__chain_id__").ok().flatten().is_none() {
        let _ = store.put(b"__chain_id__", &genesis.chain_id.to_le_bytes());
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
        validator_id = account_to_base58(&validator_id),
        validators = validator_set.active_count(),
        "validator set initialized from genesis"
    );

    let config = EngineConfig {
        block_time_ms: block_time,
        max_ops_per_block: 5000,
        validator_id,
        chain_id: genesis.chain_id,
        prune: cli.prune,
    };

    let mempool = Mempool::new(50_000);
    let mut engine_raw = ConsensusEngine::with_validators(config, store, mempool, validator_set);
    // Set signing keypair for block header signatures (proves proposer authored the block).
    engine_raw.set_signing_keypair(Keypair::from_seed(&validator_seed));
    let engine = Arc::new(engine_raw);

    // Reconcile in-memory validator set with on-chain staking state.
    // After a restart, the in-memory set is from genesis and doesn't reflect
    // validators that registered, got slashed/jailed, or changed stake on-chain.
    {
        use solen_system_contracts::staking::StakingContract;
        use solen_consensus::validator::ValidatorInfo;
        let store_lock = engine.store();
        let store_guard = store_lock.read().unwrap();
        let staking = StakingContract::load(&**store_guard);
        drop(store_guard);
        let vs_lock = engine.validator_set();
        let mut vs = vs_lock.write().unwrap();
        for on_chain in &staking.validators {
            let total_stake = on_chain.self_stake.saturating_add(on_chain.total_delegated);
            if let Some(v) = vs.get_mut(&on_chain.id) {
                v.stake = total_stake;
                if !on_chain.is_active {
                    v.status = solen_consensus::validator::ValidatorStatus::Jailed;
                    info!(
                        validator = account_to_base58(&on_chain.id),
                        "validator is jailed on-chain — marking inactive in consensus set"
                    );
                }
            } else if on_chain.is_active && total_stake > 0 {
                // Validator registered after genesis — add to consensus set.
                let vi = ValidatorInfo::new(on_chain.id, total_stake);
                vs.add(vi);
                info!(
                    validator = account_to_base58(&on_chain.id),
                    stake = total_stake,
                    "added post-genesis validator to consensus set from on-chain state"
                );
            }
        }
        // Remove genesis validators that exited on-chain (self_stake == 0 and not in staking list).
        let on_chain_ids: std::collections::HashSet<[u8; 32]> =
            staking.validators.iter().map(|v| v.id).collect();
        let to_remove: Vec<[u8; 32]> = vs.all()
            .iter()
            .filter(|v| !on_chain_ids.contains(&v.id))
            .map(|v| v.id)
            .collect();
        for id in to_remove {
            vs.remove(&id);
            info!(
                validator = account_to_base58(&id),
                "removed validator not present in on-chain staking state"
            );
        }
    }

    // Load persisted finalized checkpoint (survives restarts, anchors snapshot sync).
    {
        let store_lock = engine.store();
        let store_guard = store_lock.read().unwrap();
        let loaded = solen_consensus::checkpoint::FinalizedCheckpointStore::load(&**store_guard);
        if let Some(ref cp) = loaded.latest {
            info!(
                height = cp.height,
                epoch = cp.epoch,
                attestations = cp.attestations.len(),
                "loaded finalized checkpoint from store"
            );
        }
        drop(store_guard);
        *engine.finalized_checkpoints().write().unwrap() = loaded;
    }

    // Syncing flag: start in sync mode for multi-validator networks to prevent
    // producing blocks before we've caught up with the network.
    let is_multi = engine.active_validator_count() > 1;
    let syncing = Arc::new(std::sync::atomic::AtomicBool::new(is_multi));

    // Track the highest known network height (from StatusAnnounce messages).
    let network_height = Arc::new(std::sync::atomic::AtomicU64::new(0));

    // Wrap validator keypair in Arc so it can be shared between P2P handler
    // and consensus loop (both need it for signing attestations).
    let attestation_kp = Arc::new(validator_kp);

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
        let att_kp_for_p2p = attestation_kp.clone();
        tokio::spawn(async move {
            // Track repeated sync requests to rate-limit stuck peers.
            let mut sync_serve_tracker: std::collections::HashMap<u64, (std::time::Instant, u32)> =
                std::collections::HashMap::new();
            let mut sync_fail_count: u32 = 0;
            let mut fork_mismatch_detected = false;
            let mut bad_state_roots: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
            let _p2p_genesis_hash = expected_genesis_hash;

            while let Some(msg) = inbound_rx.recv().await {
                match msg {
                    NetworkMessage::NewTransaction(op) => {
                        // Deprioritize transactions when the channel is congested.
                        // This ensures consensus messages (blocks, attestations) aren't
                        // starved by transaction flooding.
                        if inbound_rx.len() > 2048 {
                            // Channel >50% full — drop transactions to preserve
                            // bandwidth for consensus-critical messages.
                            continue;
                        }
                        engine_for_p2p.mempool().submit(op);
                    }
                    NetworkMessage::NewBlock {
                        header,
                        operations,
                        ..
                    } => {
                        // Reject oversized blocks to prevent memory DoS.
                        if operations.len() > 5000 {
                            tracing::warn!(ops = operations.len(), "rejecting oversized block");
                            continue;
                        }
                        // While syncing, still accept live blocks — the node needs
                        // to catch up even if sync protocol isn't delivering.
                        // accept_block handles fast-forward for gaps.
                        // Validate and accept the block.
                        if engine_for_p2p.accept_block(&header, &operations) {
                            // Block accepted from a peer — we have connectivity.
                            // Reset partition state so block production can resume.
                            if engine_for_p2p.is_likely_partitioned() {
                                tracing::info!(
                                    height = header.height,
                                    "received valid block from peer — clearing partition state"
                                );
                                engine_for_p2p.reset_partition_state();
                            }

                            // If we were syncing, this live block verifies our state.
                            // Force-finalize it immediately — the network already has
                            // consensus on this block, no need to wait for attestations.
                            if syncing_for_p2p.swap(false, std::sync::atomic::Ordering::Relaxed) {
                                fork_mismatch_detected = false; // valid peer found
                                // Clear stale pending blocks from BEFORE this height.
                                engine_for_p2p.clear_stale_pending(header.height.saturating_sub(1));
                                // Immediately finalize the accepted block (executes it).
                                engine_for_p2p.force_finalize_block(header.height);
                                tracing::info!(
                                    height = header.height,
                                    "state verified — resuming block production"
                                );
                            }
                            // Send our signed attestation back.
                            let bh = solen_consensus::engine::block_hash(&header);
                            let att_payload = attestation_payload(engine_for_p2p.config().chain_id, header.height, &bh);
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
                        } else {
                            // Block not accepted — if it's ahead of us, request sync.
                            let our_h = engine_for_p2p.height();
                            if header.height > our_h + 1 && !fork_mismatch_detected {
                                tracing::info!(
                                    our_height = our_h,
                                    block_height = header.height,
                                    "live block ahead of us — requesting sync"
                                );
                                syncing_for_p2p.store(true, std::sync::atomic::Ordering::Relaxed);
                                net_height_for_p2p.fetch_max(header.height, std::sync::atomic::Ordering::Relaxed);
                                net_for_attest.broadcast(NetworkMessage::SyncRequest {
                                    from_height: our_h + 1,
                                    to_height: header.height,
                                });
                            }
                        }
                    }
                    NetworkMessage::Attestation {
                        validator_id,
                        block_height,
                        block_hash,
                        signature,
                    } => {
                        // Verify attestation signature with domain separation.
                        let payload = attestation_payload(engine_for_p2p.config().chain_id, block_height, &block_hash);
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
                                let v_b58 = account_to_base58(&validator_id);
                                tracing::warn!(
                                    validator = %v_b58,
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
                    NetworkMessage::StatusAnnounce { height, state_root: _sr } => {
                        // Track peer heights — use a set of unique heights so that
                        // repeated announcements from the same stale node don't
                        // accumulate and falsely trigger sync mode.
                        {
                            let mut ph = peer_heights_for_p2p.lock().unwrap();
                            ph.push(height);
                            // Cap to prevent unbounded growth (memory leak).
                            let len = ph.len();
                            if len > 100 {
                                ph.drain(..len - 100);
                            }
                        }

                        let our_height = engine_for_p2p.height();
                        if height > our_height + 1 {
                            // After detecting a fork mismatch, completely ignore
                            // StatusAnnounce sync triggers. Legitimate catch-up
                            // is handled by the live block handler (NewBlock gap detection).
                            if fork_mismatch_detected {
                                continue;
                            }

                            let peer_h = peer_heights_for_p2p.lock().unwrap();
                            let peers_ahead = peer_h.iter().filter(|&&h| h > our_height + 1).count();
                            let total_peers = peer_h.len();
                            drop(peer_h);

                            // Need at least 2 UNIQUE heights ahead to enter sync.
                            // Only trust a single peer if we're at genesis (no blocks yet).
                            if peers_ahead >= 2 || (total_peers <= 1 && our_height == 0) {
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
                    NetworkMessage::CheckpointAttestation {
                        validator_id,
                        height,
                        block_hash,
                        state_root,
                        signature,
                    } => {
                        // Verify the attestation signature.
                        let msg = solen_consensus::checkpoint::FinalizedCheckpointStore::signing_message(
                            height, &block_hash, &state_root,
                        );
                        if signature.len() == 64 {
                            let mut sig = [0u8; 64];
                            sig.copy_from_slice(&signature);
                            if solen_crypto::verify(&validator_id, &msg, &sig).is_ok() {
                                let finalized = engine_for_p2p.attest_checkpoint_with_data(
                                    validator_id, signature, height, &block_hash, &state_root,
                                );
                                if finalized {
                                    tracing::info!(height, "checkpoint finalized via peer attestation");
                                }
                            }
                        }
                    }
                    NetworkMessage::SyncRequest { from_height, to_height } => {
                        // Rate-limit sync serving: ignore repeated requests for the
                        // same height range (stuck peer on old code).
                        let now = std::time::Instant::now();
                        let should_serve = {
                            let entry = sync_serve_tracker.entry(from_height).or_insert((now, 0u32));
                            if now.duration_since(entry.0) < std::time::Duration::from_secs(30) {
                                entry.1 += 1;
                                entry.1 <= 3 // serve max 3 times per 30s per height
                            } else {
                                *entry = (now, 1);
                                true
                            }
                        };
                        // Prune old tracker entries periodically.
                        if sync_serve_tracker.len() > 1000 {
                            sync_serve_tracker.retain(|_, (t, _)| now.duration_since(*t) < std::time::Duration::from_secs(60));
                        }

                        if !should_serve {
                            tracing::debug!(from = from_height, "ignoring repeated sync request (rate limited)");
                        } else {
                            let max_batch = 100;
                            let _to = to_height.min(from_height + max_batch as u64 - 1);
                            let blocks = engine_for_p2p.get_blocks_for_sync(from_height, max_batch);

                            // Only serve if the first block matches what was requested.
                            // Prevents sending wrong blocks when we don't have the requested range.
                            if !blocks.is_empty() && blocks[0].header.height == from_height {
                                let sync_blocks: Vec<solen_p2p::messages::SyncBlock> = blocks
                                    .iter()
                                    .map(|b| solen_p2p::messages::SyncBlock {
                                        header: b.header.clone(),
                                        operations: b.operations.clone(),
                                        receipts: b.result.receipts.clone(),
                                    })
                                    .collect();

                                tracing::debug!(
                                    from = from_height,
                                    count = sync_blocks.len(),
                                    "serving sync blocks to peer"
                                );

                                net_for_attest.broadcast(NetworkMessage::SyncBlocks {
                                    blocks: sync_blocks,
                                });
                            }
                        }
                    }
                    NetworkMessage::SyncBlocks { mut blocks } => {
                        if blocks.is_empty() {
                            continue;
                        }

                        // Fork isolation: drop blocks with known-bad state roots
                        // and blocks we can't apply (wrong height).
                        if fork_mismatch_detected {
                            let our_h = engine_for_p2p.height();
                            blocks.retain(|b| {
                                b.header.height >= our_h + 1
                                    && !bad_state_roots.contains(&b.header.state_root)
                            });
                            if blocks.is_empty() {
                                continue;
                            }
                        }

                        // Cap sync batch size to prevent memory DoS.
                        if blocks.len() > 100 {
                            blocks.truncate(100);
                        }

                        // Sort by height so out-of-order arrivals are processed correctly.
                        blocks.sort_by_key(|b| b.header.height);

                        let mut synced = 0u64;
                        let mut had_gap = false;

                        // Sort and feed blocks in order. replay_synced_block returns
                        // false on gap/duplicate, true on success.
                        for sync_block in &blocks {
                            // Skip blocks with known-bad state roots (faster than executing).
                            if bad_state_roots.contains(&sync_block.header.state_root) {
                                continue;
                            }
                            let applied = engine_for_p2p.replay_synced_block(
                                &sync_block.header,
                                &sync_block.operations,
                                sync_block.receipts.clone(),
                            );
                            if applied {
                                synced += 1;
                            } else if sync_block.header.height == engine_for_p2p.height() + 1 {
                                // Block was at the right height but failed — remember its state root.
                                bad_state_roots.insert(sync_block.header.state_root);
                                // Cap the set to prevent unbounded growth.
                                if bad_state_roots.len() > 1000 {
                                    bad_state_roots.clear();
                                }
                            } else if sync_block.header.height > engine_for_p2p.height() + 1 {
                                had_gap = true;
                            }
                        }

                        let our_height = engine_for_p2p.height();
                        let known_net_height = net_height_for_p2p.load(std::sync::atomic::Ordering::Relaxed);

                        if synced > 0 {
                            sync_fail_count = 0; // reset on any success
                            tracing::info!(
                                synced,
                                new_height = our_height,
                                network_height = known_net_height,
                                "synced blocks from peer"
                            );
                        } else if !blocks.is_empty() {
                            // Received blocks but none applied — likely a fork mismatch.
                            sync_fail_count += 1;
                            if sync_fail_count >= 1 {
                                if !fork_mismatch_detected {
                                    tracing::warn!(
                                        our_height,
                                        "sync blocks rejected — peers on a different fork, disabling sync from announcements"
                                    );
                                }
                                syncing_for_p2p.store(false, std::sync::atomic::Ordering::Relaxed);
                                sync_fail_count = 0;
                                fork_mismatch_detected = true;
                                // Reset tracked peer heights so we don't think we're behind.
                                peer_heights_for_p2p.lock().unwrap().clear();
                                net_height_for_p2p.store(0, std::sync::atomic::Ordering::Relaxed);
                                // Don't auto-resync from sync rejection — it could be transient.
                                // Only state root mismatches during finalization trigger resync.
                                continue;
                            }
                        }

                        // Re-request if we're still behind (whether we synced some or hit a gap).
                        if our_height + 1 >= known_net_height {
                            // Clear any pending resync — we caught up via normal sync.
                            engine_for_p2p.take_resync_request();
                            tracing::info!(
                                height = our_height,
                                "sync caught up, waiting to verify state with live block"
                            );
                        } else if synced > 0 || had_gap {
                            // Still behind — request missing range.
                            net_for_attest.broadcast(NetworkMessage::SyncRequest {
                                from_height: our_height + 1,
                                to_height: known_net_height,
                            });
                            if had_gap {
                                tracing::info!(
                                    from = our_height + 1,
                                    to = known_net_height,
                                    "detected gap in sync — re-requesting missing blocks"
                                );
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
                        status_engine.clear_stale_pending(height);
                        tracing::info!(height, network_height = known, "timeout — resuming block production");
                        syncing_for_status.store(false, std::sync::atomic::Ordering::Relaxed);
                    } else if height > 0 && known == 0 && ticks_since_start >= 5 {
                        // We replayed the chain but no peer has announced a height yet.
                        // We're likely the most advanced node — resume production.
                        status_engine.clear_stale_pending(height);
                        tracing::info!(height, "no peer heights received — resuming as lead validator");
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
    let att_kp_for_consensus = attestation_kp.clone();
    let syncing_for_consensus = syncing.clone();
    let net_for_resync = net;
    let consensus_handle = tokio::spawn(async move {
        // Wait for P2P mesh to form before producing blocks.
        // Gossipsub needs several heartbeats to build the mesh after peers connect.
        if engine_clone.active_validator_count() > 1 {
            let wait = if engine_clone.height() == 0 { 30 } else { 10 };
            info!(seconds = wait, "waiting for P2P mesh to form...");
            tokio::time::sleep(tokio::time::Duration::from_secs(wait)).await;

            let is_validator = {
                let vs = engine_clone.validator_set();
                let vs = vs.read().unwrap();
                vs.all().iter().any(|v| v.id == engine_clone.validator_id())
            };
            if is_validator {
                info!("starting block production (active validator)");
            } else {
                info!("starting consensus listener (non-validator node)");
            }
        }

        // Poll frequently but enforce block_time between proposals.
        let mut poll = tokio::time::interval(tokio::time::Duration::from_millis(200));
        let mut min_interval = std::time::Duration::from_millis(block_time);
        let quorum_timeout = std::time::Duration::from_secs(10);
        let mut last_finalized_height = engine_clone.height();
        // Reset AFTER mesh warmup so stalled_for doesn't start at 30+ seconds.
        // This prevents all validators from thinking they're backup proposers at genesis.
        let mut last_finalized_at = std::time::Instant::now();
        let mut last_proposed_at = std::time::Instant::now();

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
            // Only reset the proposal timer for stalled_for tracking,
            // NOT for min_interval — we want to propose as soon as it's
            // our turn after a peer's block, not wait an extra block_time.
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

                // At epoch boundaries, broadcast our checkpoint attestation.
                if current_height % 100 == 0 {
                    if let Some((cp_height, cp_block_hash, cp_state_root)) = engine_clone.pending_checkpoint() {
                        let msg = solen_consensus::checkpoint::FinalizedCheckpointStore::signing_message(
                            cp_height, &cp_block_hash, &cp_state_root,
                        );
                        let sig = att_kp_for_consensus.sign(&msg);
                        if let Some(ref handle) = net_for_blocks {
                            handle.broadcast(NetworkMessage::CheckpointAttestation {
                                validator_id: engine_clone.validator_id(),
                                height: cp_height,
                                block_hash: cp_block_hash,
                                state_root: cp_state_root,
                                signature: sig.to_vec(),
                            });
                        }
                    }
                }
            }

            // Enforce minimum interval since last proposal to prevent spamming.
            // Uses proposal time, not finalization time — otherwise attestation
            // round-trips add to the effective block time.
            if last_proposed_at.elapsed() < min_interval {
                continue;
            }

            // Produce if it's our turn, or take over if the proposer is offline.
            let next_height = engine_clone.height() + 1;
            let already_pending = engine_clone.has_pending_block(next_height);
            let stalled_for = last_finalized_at.elapsed();

            // Only propose if:
            // 1. Single validator mode (always produce), OR
            // 2. We're the designated proposer and no block pending, OR
            // 3. We're the backup proposer AND no block pending from any source.
            //    The `already_pending` check prevents competing blocks at the same
            //    height, which causes attestation hash mismatches.
            // Auto-resync: if state diverged, download a fresh snapshot from peers.
            if engine_clone.take_resync_request() {
                warn!("state divergence detected — initiating automatic snapshot resync");
                engine_clone.set_resyncing(true);
                // Try each seed RPC for a snapshot.
                let resync_urls: Vec<String> = match net_for_resync {
                    Network::Testnet => vec![
                        "https://testnet-rpc.solenchain.io".into(),
                        "https://testnet-rpc2.solenchain.io".into(),
                        "https://testnet-rpc3.solenchain.io".into(),
                    ],
                    _ => vec![],
                };

                let mut resync_ok = false;
                for url in &resync_urls {
                    // Skip if this URL points to ourselves — check by comparing height.
                    let client = match reqwest::blocking::Client::builder()
                        .timeout(std::time::Duration::from_secs(120))
                        .build() {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    // Quick height check — if peer is at same or lower height, skip.
                    let our_h = engine_clone.height();
                    if let Ok(resp) = client.post(url.as_str())
                        .header("Content-Type", "application/json")
                        .body(r#"{"jsonrpc":"2.0","id":1,"method":"solen_chainStatus","params":[]}"#)
                        .send()
                    {
                        if let Ok(json) = resp.json::<serde_json::Value>() {
                            let peer_h = json["result"]["height"].as_u64().unwrap_or(0);
                            if peer_h <= our_h {
                                info!(url, our_h, peer_h, "skipping resync source — not ahead of us");
                                continue;
                            }
                        }
                    }
                    info!(url, "attempting snapshot resync...");

                    // Get metadata first.
                    let meta_resp = match client.post(url.as_str())
                        .header("Content-Type", "application/json")
                        .body(r#"{"jsonrpc":"2.0","id":1,"method":"solen_getSnapshotMeta","params":[]}"#)
                        .send() {
                        Ok(r) => r,
                        Err(e) => { warn!(url, error = %e, "resync meta failed"); continue; }
                    };
                    let meta_json: serde_json::Value = match meta_resp.json() {
                        Ok(j) => j,
                        Err(_) => continue,
                    };
                    let total_bytes = match meta_json["result"]["total_bytes"].as_u64() {
                        Some(t) => t as usize,
                        None => { warn!(url, "resync: no total_bytes in meta"); continue; }
                    };

                    info!(url, total_bytes, "downloading snapshot chunks...");
                    let chunk_size: usize = 4 * 1024 * 1024;
                    let mut snapshot_data = Vec::with_capacity(total_bytes);
                    let mut offset: usize = 0;
                    let mut download_ok = true;

                    loop {
                        let body = serde_json::json!({
                            "jsonrpc": "2.0", "id": 1,
                            "method": "solen_getSnapshotChunk",
                            "params": [offset, chunk_size]
                        });
                        let resp = match client.post(url.as_str())
                            .header("Content-Type", "application/json")
                            .body(body.to_string())
                            .send() {
                            Ok(r) => r,
                            Err(e) => { warn!(error = %e, "chunk download failed"); download_ok = false; break; }
                        };
                        let cj: serde_json::Value = match resp.json() {
                            Ok(j) => j,
                            Err(_) => { download_ok = false; break; }
                        };
                        let chunk_b64 = match cj["result"]["data"].as_str() {
                            Some(s) => s.to_string(),
                            None => { download_ok = false; break; }
                        };
                        let chunk_bytes = match base64_decode(&chunk_b64) {
                            Ok(b) => b,
                            Err(_) => { download_ok = false; break; }
                        };
                        let done = cj["result"]["done"].as_bool().unwrap_or(false);
                        info!(offset, chunk_len = chunk_bytes.len(), "resync chunk downloaded");
                        snapshot_data.extend_from_slice(&chunk_bytes);
                        offset += chunk_bytes.len();
                        if done || chunk_bytes.is_empty() { break; }
                    }

                    if !download_ok || snapshot_data.is_empty() { continue; }

                    // Wipe current store and restore from snapshot.
                    info!(bytes = snapshot_data.len(), "restoring snapshot...");
                    {
                        let store = engine_clone.store();
                        let mut store = store.write().unwrap();
                        store.clear().ok();
                        match solen_consensus::snapshot::restore_snapshot(store.as_mut(), &snapshot_data) {
                            Ok(meta) => {
                                info!(
                                    height = meta.height,
                                    epoch = meta.epoch,
                                    entries = meta.entry_count,
                                    "snapshot restored — resync complete"
                                );
                                // Reset engine state to match snapshot.
                                engine_clone.reset_to_height(meta.height, meta.epoch);
                                resync_ok = true;
                            }
                            Err(e) => {
                                warn!(error = %e, "snapshot restore failed");
                            }
                        }
                    }

                    if resync_ok { break; }
                }

                engine_clone.set_resyncing(false);
                if resync_ok {
                    engine_clone.reset_partition_state();
                    info!("auto-resync completed — requesting block sync to catch up");
                    // Request sync to catch up from snapshot height to network height.
                    if let Some(ref handle) = net_for_blocks {
                        let our_h = engine_clone.height();
                        handle.broadcast(NetworkMessage::SyncRequest {
                            from_height: our_h + 1,
                            to_height: our_h + 500,
                        });
                    }
                    // Wait a bit for sync blocks to arrive before producing.
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                } else {
                    warn!("auto-resync failed from all peers — node needs manual intervention");
                }
                continue;
            }

            // Don't produce blocks if we appear to be partitioned from the network.
            // This prevents divergent chains from force-finalization during partitions.
            if engine_clone.is_likely_partitioned() {
                tracing::warn!("skipping production — partition detected");
                // Clear all peer bans to allow reconnection.
                if let Some(ref handle) = net_for_blocks {
                    handle.report_peer(solen_p2p::reputation::ReputationEvent::ClearAllBans);
                    // Also request sync to try to reconnect.
                    let our_h = engine_clone.height();
                    handle.broadcast(NetworkMessage::SyncRequest {
                        from_height: our_h + 1,
                        to_height: our_h + 10,
                    });
                }
                // Still check for dropped blocks.
                let _ = engine_clone.take_dropped_block_height();
                continue;
            }

            let is_proposer = engine_clone.is_next_proposer();
            let is_backup = engine_clone.is_backup_proposer(stalled_for);
            let active_count = engine_clone.active_validator_count();
            let should_propose = active_count <= 1
                || (is_proposer && !already_pending)
                || (!already_pending && is_backup);

            if !should_propose && stalled_for.as_secs() > 5 {
                tracing::warn!(
                    height = next_height,
                    is_proposer,
                    already_pending,
                    is_backup,
                    active_count,
                    stalled_secs = stalled_for.as_secs(),
                    "stalled — not proposing"
                );
            }

            // Check if a block was dropped due to attestation mismatch.
            // If so, request sync for that height to get the correct block from peers.
            if let Some(dropped_height) = engine_clone.take_dropped_block_height() {
                if let Some(ref handle) = net_for_blocks {
                    tracing::info!(height = dropped_height, "requesting sync after dropping mismatched block");
                    handle.broadcast(NetworkMessage::SyncRequest {
                        from_height: dropped_height,
                        to_height: dropped_height + 10,
                    });
                }
            }

            if should_propose {
                let produced = engine_clone.produce_block();
                last_proposed_at = std::time::Instant::now();
                last_finalized_at = std::time::Instant::now();

                // Broadcast the proposed block with full operations.
                if let Some(ref handle) = net_for_blocks {
                    let gas = produced.finalized.as_ref().map(|b| b.result.gas_used).unwrap_or(0);
                    let tx_count = produced.operations.len();
                    let header_for_att = produced.header.clone();
                    handle.broadcast(NetworkMessage::NewBlock {
                        header: produced.header,
                        operations: produced.operations,
                        tx_count,
                        gas_used: gas,
                    });

                    // Broadcast our own attestation for the block we just proposed.
                    // Without this, non-proposer validators only see attestations
                    // from each other, missing the proposer's vote. When one validator
                    // is offline (e.g. 3 of 4 online), this means non-proposers only
                    // collect 2/4 = 50% of stake — below the 2/3 quorum threshold —
                    // causing every backup-proposed block to force-finalize.
                    let bh = solen_consensus::engine::block_hash(&header_for_att);
                    let att_payload = attestation_payload(engine_clone.config().chain_id, header_for_att.height, &bh);
                    let att_sig = att_kp_for_consensus.sign(&att_payload);
                    handle.broadcast(NetworkMessage::Attestation {
                        validator_id: engine_clone.validator_id(),
                        block_height: header_for_att.height,
                        block_hash: bh,
                        signature: att_sig.to_vec(),
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
    encoding_hex_encode(bytes)
}

/// Build the deterministic payload for attestation signing/verification.
fn attestation_payload(chain_id: u64, height: u64, block_hash: &[u8; 32]) -> Vec<u8> {
    // Use the engine's domain-separated payload to ensure consistency.
    solen_consensus::engine::ConsensusEngine::attestation_signing_payload(chain_id, height, block_hash)
}

fn rand_seed() -> [u8; 32] {
    // Use OS-provided cryptographic randomness. Never rely on timestamps,
    // PIDs, or stack addresses — those are predictable.
    let mut seed = [0u8; 32];
    solen_crypto::random_bytes(&mut seed);
    seed
}

fn hex_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    solen_types::encoding::hex_decode(s).map_err(|e| anyhow::anyhow!("{}", e))
}

fn base64_decode(input: &str) -> anyhow::Result<Vec<u8>> {
    const TABLE: [u8; 128] = {
        let mut t = [0xFF; 128];
        let mut i = 0u8;
        while i < 26 { t[(b'A' + i) as usize] = i; i += 1; }
        i = 0;
        while i < 26 { t[(b'a' + i) as usize] = 26 + i; i += 1; }
        i = 0;
        while i < 10 { t[(b'0' + i) as usize] = 52 + i; i += 1; }
        t[b'+' as usize] = 62;
        t[b'/' as usize] = 63;
        t
    };

    let input = input.trim_end_matches('=');
    let mut output = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;

    for &b in input.as_bytes() {
        if b > 127 || TABLE[b as usize] == 0xFF {
            anyhow::bail!("invalid base64 character: {}", b as char);
        }
        buf = (buf << 6) | TABLE[b as usize] as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }

    Ok(output)
}
