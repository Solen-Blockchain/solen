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
use solen_storage::StateStore;
use solen_types::encoding::{account_to_base58, hex_encode as encoding_hex_encode};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

/// Number of local snapshot checkpoints to retain for fast fork recovery.
/// Written every SNAPSHOT_CACHE_INTERVAL (~500) blocks, so this covers roughly
/// the last `LOCAL_SNAPSHOT_KEEP * 500` blocks of rollback range.
const LOCAL_SNAPSHOT_KEEP: usize = 3;

/// Native RocksDB checkpoints (hard-linked, cheap) for fine-grained,
/// restart-surviving local fork recovery. Created every
/// `ROCKS_CHECKPOINT_INTERVAL` blocks, keeping the newest `ROCKS_CHECKPOINT_KEEP`
/// (≈ keep × interval blocks of coverage). Hard links share SSTs, so the on-disk
/// cost is far below that many full copies.
const ROCKS_CHECKPOINT_KEEP: usize = 8;
const ROCKS_CHECKPOINT_INTERVAL: u64 = 100;

/// Sync-starved strand auto-recovery thresholds. A node that is at least
/// `STRANDED_BEHIND_BLOCKS` behind a live network and whose height has not
/// advanced for `STRANDED_RESYNC_AFTER` is wedged: block-sync is requesting but
/// not delivering blocks (observed on mainnet 2026-06-25 — a fallen-behind
/// validator could not fetch the epoch-transition block via sync and froze).
/// Neither the finalize-path nor the fork-mismatch resync covers this, because
/// both require `replay_synced_block` to be reached and here no sync block is
/// ever applied. The "behind" margin is well beyond normal sync lag (a few
/// blocks); the freeze timer means a node that IS catching up never trips it.
const STRANDED_BEHIND_BLOCKS: u64 = 12;
const STRANDED_RESYNC_AFTER: std::time::Duration = std::time::Duration::from_secs(40);

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

    /// Canonical RPC URL(s) to recover from when this node diverges (repeatable).
    /// Tried before the network's built-in defaults. On devnet there are no
    /// defaults, so this is what enables auto-recovery (and the partition drill)
    /// in an isolated cluster — point it at a peer's RPC.
    #[arg(long)]
    resync_url: Vec<String>,
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
                "info,libp2p_gossipsub::peer_score=error,libp2p_gossipsub::behaviour=warn,libp2p_kad::handler=error"
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
    // Snapshot sync uses reqwest::blocking, whose internal runtime cannot be
    // dropped inside the #[tokio::main] async context without panicking
    // ("Cannot drop a runtime in a context where blocking is not allowed").
    // Run the whole blocking section under block_in_place, which marks this
    // worker thread as allowed to block (and to drop that runtime) — so an
    // unreachable seed now yields a normal error instead of crashing the node.
    tokio::task::block_in_place(|| -> anyhow::Result<()> {
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

                // Step 3: Verify the finalized checkpoint via lightweight metadata
                // and select a seed URL. The actual download happens in the unified
                // snapshot-load path below, which fetches via chunked transfer.
                //
                // We deliberately do NOT download here with the single-call
                // `solen_getSnapshot`: a full mainnet snapshot base64-encodes well
                // past the RPC max response body size (100 MB), so the single call
                // always fails and the node would silently fall back to genesis
                // block-sync. `getSnapshotMeta` returns the state root + finalized
                // checkpoint (cheap), which is all we need to validate before
                // streaming the body in 4 MB chunks.
                let meta_body = serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "method": "solen_getSnapshotMeta", "params": []
                });

                // Build the trusted genesis validator set (public_key -> stake)
                // for verifying snapshot checkpoint attestations. The genesis keys
                // are the ONLY validator set we can trust without already having a
                // snapshot, so they anchor long-range-attack protection: a forged
                // snapshot cannot fabricate a checkpoint carrying valid signatures
                // from a 2/3 stake supermajority of these keys.
                //
                // This assumes the active validator set still descends from genesis
                // (true on Solen mainnet: all 11 active validators are the genesis
                // validators). If staking ever rotates the set away from genesis,
                // this must evolve to track validator-set changes through a verified
                // epoch/header chain rather than trusting genesis alone.
                let genesis_vset: Vec<([u8; 32], u128)> = {
                    let mut vs = Vec::with_capacity(genesis.validators.len());
                    for v in &genesis.validators {
                        let pk = if let Some(seed_hex) = &v.seed_hex {
                            let bytes = hex_decode(seed_hex)?;
                            let mut arr = [0u8; 32];
                            arr.copy_from_slice(&bytes);
                            Keypair::from_seed(&arr).public_key()
                        } else if let Some(pk_hex) = &v.public_key_hex {
                            let bytes = hex_decode(pk_hex)?;
                            let mut arr = [0u8; 32];
                            arr.copy_from_slice(&bytes);
                            arr
                        } else {
                            anyhow::bail!("validator '{}' needs seed_hex or public_key_hex", v.name);
                        };
                        vs.push((pk, v.stake));
                    }
                    vs
                };

                for (url, _, _) in &at_max {
                    info!(url = %url, "fetching snapshot metadata...");
                    match client.post(url.as_str())
                        .header("Content-Type", "application/json")
                        .body(meta_body.to_string()).send()
                    {
                        Ok(resp) if resp.status().is_success() => {
                            if let Ok(json) = resp.json::<serde_json::Value>() {
                                let snap_root = json["result"]["state_root"].as_str().unwrap_or("");

                                // The merkle root is re-verified against the header in
                                // restore_snapshot() after decompression; here we only
                                // log if the (possibly cached) snapshot root differs.
                                if !snap_root.is_empty() && snap_root != consensus_root {
                                    info!(
                                        snap_root,
                                        consensus_root = %consensus_root,
                                        "snapshot root differs from consensus (may be cached) — will verify after restore"
                                    );
                                }

                                // Verify the finalized checkpoint if present. The
                                // checkpoint sits at cp_height <= the snapshot height, so
                                // its state_root legitimately differs from the snapshot tip
                                // root (snap_root) whenever state changed after it — so we do
                                // NOT compare the two (that was the old bug that rejected
                                // every real snapshot). Instead we cryptographically verify
                                // that a 2/3 stake supermajority of the trusted GENESIS
                                // validators actually signed this checkpoint. Combined with
                                // the post-restore merkle re-derivation of snap_root, this
                                // means a forged snapshot cannot be accepted unless the
                                // attacker also holds 2/3 of the genesis signing keys.
                                let checkpoint_valid = if let Some(cp) = json["result"]["checkpoint"].as_object() {
                                    let cp_state_root = cp.get("state_root")
                                        .and_then(|v| v.as_str()).unwrap_or("");
                                    let cp_block_hash = cp.get("block_hash")
                                        .and_then(|v| v.as_str()).unwrap_or("");
                                    let cp_height = cp.get("height")
                                        .and_then(|v| v.as_u64()).unwrap_or(0);
                                    let attestations = cp.get("attestations")
                                        .and_then(|v| v.as_array());

                                    // Rebuild the exact message the validators signed:
                                    // signing_message(height, block_hash, state_root).
                                    let signing_msg = match (hex_decode(cp_block_hash), hex_decode(cp_state_root)) {
                                        (Ok(bh), Ok(sr)) if bh.len() == 32 && sr.len() == 32 => {
                                            let mut bha = [0u8; 32]; bha.copy_from_slice(&bh);
                                            let mut sra = [0u8; 32]; sra.copy_from_slice(&sr);
                                            Some(solen_consensus::checkpoint::FinalizedCheckpointStore::signing_message(
                                                cp_height, &bha, &sra,
                                            ))
                                        }
                                        _ => None,
                                    };

                                    match (signing_msg, attestations) {
                                        (Some(msg), Some(atts)) => {
                                            // Sum the genesis stake of every distinct genesis
                                            // validator whose signature over this checkpoint
                                            // verifies. Dedupe so a repeated attester (or a
                                            // proposer self-attestation) can't be double-counted.
                                            let mut counted: Vec<[u8; 32]> = Vec::new();
                                            for att in atts {
                                                let (vb58, sig_hex) = match att.as_array() {
                                                    Some(p) => (
                                                        p.first().and_then(|x| x.as_str()).unwrap_or(""),
                                                        p.get(1).and_then(|x| x.as_str()).unwrap_or(""),
                                                    ),
                                                    None => continue,
                                                };
                                                // Match attester to a genesis validator by base58(pubkey).
                                                let pk = match genesis_vset.iter()
                                                    .find(|(pk, _)| account_to_base58(pk) == vb58)
                                                {
                                                    Some(e) => e.0,
                                                    None => continue, // not a genesis validator — ignore
                                                };
                                                if counted.contains(&pk) {
                                                    continue; // already counted this validator
                                                }
                                                let sig_bytes = match hex_decode(sig_hex) {
                                                    Ok(b) if b.len() == 64 => b,
                                                    _ => continue,
                                                };
                                                let mut sig = [0u8; 64];
                                                sig.copy_from_slice(&sig_bytes);
                                                if solen_crypto::verify(&pk, &msg, &sig).is_ok() {
                                                    counted.push(pk);
                                                }
                                            }

                                            // Require a strict MAJORITY of genesis validators to
                                            // have signed. We deliberately do NOT use a 2/3-stake
                                            // test: checkpoints finalize on 2/3 of *current* stake,
                                            // which has diverged from the equal genesis stakes, so a
                                            // legitimately finalized checkpoint can carry as few as
                                            // ~7 of 11 genesis signatures — fewer during a partition,
                                            // when checkpoints finalize with a bare quorum, which is
                                            // exactly when fast snapshot restore matters most. A
                                            // majority-of-genesis-keys floor still prevents forgery
                                            // (an attacker would need a majority of genesis signing
                                            // keys, a compromise that would already break consensus
                                            // directly) while tolerating those reduced signer counts.
                                            // Primary integrity still rests on seed consensus + the
                                            // post-restore merkle re-derivation in restore_snapshot().
                                            let quorum = counted.len().saturating_mul(2) > genesis_vset.len();
                                            if quorum {
                                                info!(
                                                    cp_height,
                                                    signers = counted.len(),
                                                    validators = genesis_vset.len(),
                                                    "snapshot checkpoint verified — majority of genesis validators signed"
                                                );
                                            } else {
                                                warn!(
                                                    cp_height,
                                                    signers = counted.len(),
                                                    validators = genesis_vset.len(),
                                                    "snapshot checkpoint lacks a majority of genesis-validator signatures — trying next seed"
                                                );
                                            }
                                            quorum
                                        }
                                        _ => {
                                            warn!("snapshot checkpoint missing/malformed signing fields — trying next seed");
                                            false
                                        }
                                    }
                                } else {
                                    // No checkpoint to anchor trust. On mainnet this is
                                    // refused outright: without a checkpoint signed by a
                                    // majority of genesis validators we cannot prove the
                                    // snapshot's authenticity, and accepting it would let a
                                    // malicious seed substitute fabricated state. Off mainnet
                                    // (devnet/testnet bootstrap) we still accept on seed
                                    // consensus + post-restore merkle, flagged loudly.
                                    if matches!(net, Network::Mainnet) {
                                        warn!("snapshot has no finalized checkpoint — REFUSING on mainnet (cannot verify authenticity), trying next seed");
                                        false
                                    } else {
                                        warn!("snapshot has no checkpoint — accepting on seed consensus only (UNVERIFIED, non-mainnet)");
                                        true
                                    }
                                };

                                if !checkpoint_valid {
                                    continue;
                                }

                                info!(
                                    url = %url,
                                    height = json["result"]["height"].as_u64().unwrap_or(0),
                                    "snapshot seed selected — will download via chunked transfer"
                                );
                                found = Some(url.clone());
                                break;
                            }
                        }
                        _ => { info!(url = %url, "snapshot metadata fetch failed, trying next..."); }
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
                // Download via chunked transfer. A full snapshot base64-encodes
                // past the RPC max response body size (100 MB), so the single-call
                // solen_getSnapshot fails for any non-trivial chain — chunked (4 MB)
                // works for any size and is the only reliable transport.
                let snapshot_url = snapshot_source.as_str();
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

    Ok(())
    })?;

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
                            // Clear fork mismatch state — this peer is on the right fork.
                            if fork_mismatch_detected {
                                tracing::info!(height = header.height, "valid block received — clearing fork mismatch state");
                                fork_mismatch_detected = false;
                            }
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
                                // Clear stale pending blocks from BEFORE this height.
                                engine_for_p2p.clear_stale_pending(header.height.saturating_sub(1));
                                // Finalize the accepted block only if it carries a 2/3
                                // attestation quorum. A single peer's block must NOT be
                                // committed unilaterally on a syncing node — without quorum
                                // the block stays pending and finalizes via the normal
                                // quorum-gated timeout path (or we re-sync).
                                if engine_for_p2p.force_finalize_block_if_quorum(header.height) {
                                    tracing::info!(
                                        height = header.height,
                                        "state verified — resuming block production"
                                    );
                                } else {
                                    tracing::info!(
                                        height = header.height,
                                        "tip accepted pending 2/3 quorum — not finalizing on a single peer"
                                    );
                                }
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
                            if header.height > our_h + 1 {
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
                            // Received blocks but none applied.
                            // Only count as fork mismatch if some blocks were at the right height
                            // (not just stale duplicates from other peers responding late).
                            let had_relevant_blocks = blocks.iter().any(|b| b.header.height >= our_height + 1);
                            if !had_relevant_blocks {
                                // All blocks were below our height — just stale duplicates, not a fork.
                                continue;
                            }
                            sync_fail_count += 1;
                            if sync_fail_count >= 3 {
                                if !fork_mismatch_detected {
                                    tracing::warn!(
                                        our_height,
                                        sync_fail_count,
                                        "sync blocks rejected — peers on a different fork, disabling sync from announcements"
                                    );
                                }
                                syncing_for_p2p.store(false, std::sync::atomic::Ordering::Relaxed);
                                sync_fail_count = 0;
                                fork_mismatch_detected = true;
                                // If the network is meaningfully AHEAD of us, our committed
                                // tip is forked and behind — normal sync can never advance us
                                // (peers keep serving canonical blocks our forked state rejects,
                                // and we're not finalizing, so the finalize-path resync never
                                // fires either). Trigger a resync so the tiered recovery
                                // (rollback → checkpoint → snapshot → remote) pulls us back onto
                                // canonical. Gated on being behind so a transient mismatch at the
                                // tip doesn't spuriously resync. This is the fork-strand escape
                                // hatch — replay_synced_block's revert counter can't reach its
                                // threshold here because bad_state_roots skips repeat-bad blocks.
                                if known_net_height > our_height + 2 && !engine_for_p2p.is_resyncing() {
                                    tracing::warn!(
                                        our_height,
                                        known_net_height,
                                        "stranded on a forked tip behind the network — triggering resync"
                                    );
                                    engine_for_p2p.request_resync();
                                }
                                // Reset tracked peer heights so we don't think we're behind.
                                peer_heights_for_p2p.lock().unwrap().clear();
                                net_height_for_p2p.store(0, std::sync::atomic::Ordering::Relaxed);
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
    // Wire up P2P broadcast for submitted transactions so non-validator
    // RPC nodes can relay transactions to the validators.
    let tx_broadcaster: Option<solen_rpc::methods::TxBroadcaster> = net_handle.as_ref().map(|h| {
        let handle = h.clone();
        let broadcaster: solen_rpc::methods::TxBroadcaster = std::sync::Arc::new(move |op| {
            handle.broadcast(NetworkMessage::NewTransaction(op));
        });
        broadcaster
    });
    // Local snapshot checkpoints for fast fork recovery (roll back from a local
    // file instead of re-downloading the whole chain). Keep the newest few.
    let checkpoints_dir = PathBuf::from(&data_dir).join("checkpoints");
    let local_snapshots = solen_consensus::snapshot::LocalSnapshots::new(
        checkpoints_dir.clone(), LOCAL_SNAPSHOT_KEEP,
    );
    let _rpc_handle = solen_rpc::server::start_rpc_server_full(
        rpc_addr, engine.clone(), tx_broadcaster, Some(local_snapshots.clone()),
    ).await?;

    // Fine-grained, restart-surviving local recovery via native RocksDB
    // checkpoints (hard-linked, cheap). Created periodically below; consulted by
    // the resync path before falling back to the serialized snapshot / remote.
    let rocks_checkpoints = solen_consensus::snapshot::RocksCheckpoints::new(
        PathBuf::from(&data_dir).join("rocks-checkpoints"), ROCKS_CHECKPOINT_KEEP,
    );
    {
        let engine_cp = engine.clone();
        let rc = rocks_checkpoints.clone();
        tokio::spawn(async move {
            let mut last_h = 0u64;
            let mut tick = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                tick.tick().await;
                let h = engine_cp.height();
                if h == 0 || h < last_h + ROCKS_CHECKPOINT_INTERVAL {
                    continue;
                }
                // Create under the store read lock so no commit advances the
                // height between reading it and snapshotting — the checkpoint
                // then matches (height, root) exactly. block_in_place because
                // checkpoint creation flushes + hard-links (blocking IO).
                let res = tokio::task::block_in_place(|| {
                    let store_arc = engine_cp.store();
                    let guard = store_arc.read().unwrap();
                    let cur_h = engine_cp.height();
                    let root = guard.state_root();
                    rc.create(&**guard, cur_h, &root).map(|_| cur_h)
                });
                // Advance last_h on either outcome so a persistent failure (e.g.
                // a non-RocksDB store, or a full disk) retries at most once per
                // interval rather than every tick.
                last_h = h;
                match res {
                    Ok(cur_h) => info!(height = cur_h, "local RocksDB checkpoint created"),
                    Err(e) => warn!(error = %e, "local RocksDB checkpoint failed"),
                }
            }
        });
    }

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
    let cli_resync_urls = cli.resync_url.clone();
    let local_snapshots_for_resync = local_snapshots.clone();
    let rocks_checkpoints_for_resync = rocks_checkpoints.clone();
    let network_height_for_consensus = network_height.clone();
    let consensus_handle = tokio::spawn(async move {
        // Wait for P2P mesh to form before producing blocks.
        // Gossipsub needs several heartbeats to build the mesh after peers connect.
        if engine_clone.active_validator_count() > 1 {
            let wait = if engine_clone.height() == 0 { 30 } else { 10 };
            info!(seconds = wait, "waiting for P2P mesh to form...");
            tokio::time::sleep(tokio::time::Duration::from_secs(wait)).await;

            let is_val = {
                let vs = engine_clone.validator_set();
                let vs = vs.read().unwrap();
                vs.all().iter().any(|v| v.id == engine_clone.validator_id())
            };
            if is_val {
                info!("starting block production (active validator)");
            } else {
                info!("starting consensus listener (non-validator node)");
            }
        }

        let is_validator = {
            let vs = engine_clone.validator_set();
            let vs = vs.read().unwrap();
            vs.all().iter().any(|v| v.id == engine_clone.validator_id())
        };

        // Poll frequently but enforce block_time between proposals.
        // 50ms keeps gate-opening latency tight (avg ~25ms slack vs ~100ms
        // at the previous 200ms cadence) without meaningful CPU cost —
        // each iteration is lightweight (height check, governance peek,
        // is_next_proposer hash, sync flag).
        let mut poll = tokio::time::interval(tokio::time::Duration::from_millis(50));
        let mut min_interval = std::time::Duration::from_millis(block_time);
        let quorum_timeout = std::time::Duration::from_secs(10);
        let mut last_finalized_height = engine_clone.height();
        // Reset AFTER mesh warmup so stalled_for doesn't start at 30+ seconds.
        // This prevents all validators from thinking they're backup proposers at genesis.
        let mut last_finalized_at = std::time::Instant::now();
        let mut last_proposed_at = std::time::Instant::now();
        // Throttle the partition-recovery SyncRequest broadcast. The production
        // loop polls every 50 ms, so an unthrottled broadcast here fires ~20×/sec
        // on every latched validator — the bandwidth storm that saturated links
        // during the 2026-06-23 partition. A reconnect is detected just as well
        // at a few-second cadence. Initialized in the past so the first probe is
        // immediate.
        let partition_sync_interval = std::time::Duration::from_secs(3);
        let mut last_partition_sync_at = std::time::Instant::now()
            .checked_sub(partition_sync_interval)
            .unwrap_or_else(std::time::Instant::now);
        // Sync-starved strand tracker: the height we were last seen at and when it
        // last changed. If it stays frozen while we're behind a live network, the
        // block-sync path is starved and we trigger a resync (see consts above).
        let mut last_stranded_height = engine_clone.height();
        let mut last_stranded_progress_at = std::time::Instant::now();

        loop {
            poll.tick().await;

            if *shutdown_rx.borrow() {
                info!("consensus engine stopping");
                break;
            }

            // --- Sync-starved strand auto-recovery ---
            // Runs BEFORE the "syncing → continue" gate below: a sync-starved node
            // IS in syncing mode (it keeps requesting sync) but its height never
            // advances, so any check placed after that gate would never run for it.
            // Detect it directly — meaningfully behind a live network with a frozen
            // height — and request a resync so the tiered recovery pulls us to the
            // tip. Gated on NOT being partition-latched (partitions self-heal via
            // the deterministic prober; an earlier ungated "behind + not advancing"
            // heuristic disrupted that) and not already resyncing.
            {
                let our_h = engine_clone.height();
                let net_h = network_height_for_consensus
                    .load(std::sync::atomic::Ordering::Relaxed);
                let stranded = our_h > 0
                    && net_h > our_h + STRANDED_BEHIND_BLOCKS
                    && !engine_clone.is_resyncing()
                    && !engine_clone.is_likely_partitioned();
                if stranded && our_h == last_stranded_height {
                    if last_stranded_progress_at.elapsed() >= STRANDED_RESYNC_AFTER {
                        warn!(
                            our_height = our_h,
                            network_height = net_h,
                            "behind network with no block-sync progress — triggering resync (sync-starved strand)"
                        );
                        engine_clone.request_resync();
                        // Debounce so we don't re-fire before the resync runs.
                        last_stranded_progress_at = std::time::Instant::now();
                    }
                } else {
                    // Advancing, caught up, partitioned, or resyncing — reset.
                    last_stranded_height = our_h;
                    last_stranded_progress_at = std::time::Instant::now();
                }
            }

            // Don't do anything consensus-related while syncing — UNLESS a resync
            // has been requested. A sync-starved node is permanently in syncing
            // mode (it keeps requesting sync that never delivers), and the resync
            // executor (take_resync_request, below) lives after this gate; without
            // this exception the sync-starved trigger would set needs_resync but
            // the loop would `continue` here every iteration and never execute the
            // resync — the flag is set and re-set forever while the node stays
            // wedged (observed on mainnet 2026-06-26: validator9 logged the trigger
            // repeatedly at 40s intervals but never resynced).
            if syncing_for_consensus.load(std::sync::atomic::Ordering::Relaxed)
                && !engine_clone.resync_requested()
            {
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
            //
            // `last_finalized_at` is reset to "now" because backup-proposer
            // logic (in is_backup_proposer) uses elapsed time since the
            // last *observed* finalization to decide if the chain has
            // stalled.
            //
            // `last_proposed_at` is reset to the BLOCK HEADER's timestamp
            // (not "now") so the proposal-spam gate measures from when the
            // block was actually produced rather than when we observed it
            // finalize. Otherwise attestation propagation + collection
            // (~1-2s on a healthy network) gets added to every block,
            // pushing effective block_time ~25% above the configured value.
            let current_height = engine_clone.height();
            if current_height > last_finalized_height {
                last_finalized_height = current_height;
                last_finalized_at = std::time::Instant::now();
                let block_ts_ms = engine_clone
                    .latest_block()
                    .map(|b| b.header.timestamp_ms)
                    .unwrap_or(0);
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let age_ms = now_ms.saturating_sub(block_ts_ms);
                last_proposed_at = std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_millis(age_ms))
                    .unwrap_or_else(std::time::Instant::now);

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
                    // Canonical mainnet RPC (load-balanced over the public RPC
                    // fleet). Without this a forked mainnet validator can't
                    // self-heal and loops forever requesting sync from peers it
                    // then rejects as a different fork — it must be wiped and
                    // resynced by hand. Listed so auto-resync works on mainnet.
                    Network::Mainnet => vec![
                        "https://rpc.solenchain.io".into(),
                    ],
                    // Devnet is local-only; no public resync source.
                    _ => vec![],
                };
                // Operator-supplied canonical RPC(s) take priority over the
                // built-in defaults (and are the ONLY source on devnet).
                let resync_urls: Vec<String> = cli_resync_urls
                    .iter()
                    .cloned()
                    .chain(resync_urls)
                    .collect();

                let mut resync_ok = false;
                // Same reqwest::blocking-in-async hazard as startup snapshot sync,
                // but here inside the spawned consensus task. block_in_place lets
                // the blocking client (and its runtime drop) run without panicking
                // — required now that mainnet has resync URLs.
                tokio::task::block_in_place(|| {
                // If our current tip is already canonical we are not forked —
                // only behind/sync-starved — and the backward recovery tiers
                // below would merely REGRESS us to an earlier canonical height,
                // from which the same starved block-sync path re-strands us (an
                // endless resync loop). Those tiers are correct only for a FORKED
                // tip; when our tip is canonical, skip straight to the
                // forward-pulling remote snapshot. (state_root collides across
                // heights on an idle chain, but it is compared at the SAME height
                // here, so the check is sound.)
                let tip_canonical = {
                    let h = engine_clone.height();
                    let root = engine_clone.store().read().unwrap().state_root();
                    h > 0 && checkpoint_is_canonical(&resync_urls, h, &root)
                };
                if !tip_canonical {
                // --- Phase 2: in-place rollback to the common ancestor. If the
                // fork is shallow enough to sit within the rollback journal, find
                // the deepest height where our state root still matches the
                // canonical chain and undo just the forked suffix — no download,
                // no snapshot restore, rewind = actual fork depth. ---
                if let Some(min_target) = engine_clone.min_rollback_target() {
                    let tip = engine_clone.height();
                    if let Some((anc_h, anc_root)) =
                        find_common_ancestor(&resync_urls, &engine_clone, min_target, tip)
                    {
                        if anc_h < tip && engine_clone.rollback_to_height(anc_h, &anc_root) {
                            info!(from = tip, to = anc_h, "in-place rollback to common ancestor — resync complete");
                            resync_ok = true;
                            return;
                        }
                    }
                }

                // --- Phase 3: restore a fine-grained local RocksDB checkpoint
                // (hard-linked, restart-surviving) whose height a canonical peer
                // confirms, then sync forward. Finer than the serialized snapshot
                // below, so it rewinds and replays less; covers the case where the
                // in-memory journal is gone (e.g. after a restart). ---
                let tip = engine_clone.height();
                for entry in rocks_checkpoints_for_resync.list().into_iter().rev()
                    .filter(|e| e.height <= tip)
                {
                    if !checkpoint_is_canonical(&resync_urls, entry.height, &entry.state_root) {
                        info!(height = entry.height, "rocks checkpoint not confirmed canonical — trying older / next tier");
                        continue;
                    }
                    info!(height = entry.height, path = %entry.db_path.display(),
                          "restoring from local RocksDB checkpoint (no download)");
                    let restored = {
                        let store = engine_clone.store();
                        let mut store = store.write().unwrap();
                        store.restore_from_checkpoint(&entry.db_path)
                    };
                    match restored {
                        Ok(()) => {
                            let (h, ep) = engine_clone.reset_to_store_meta();
                            info!(height = h, epoch = ep, "RocksDB checkpoint restored — resync complete");
                            resync_ok = true;
                            return;
                        }
                        Err(err) => warn!(error = %err, "rocks checkpoint restore failed — trying older / next tier"),
                    }
                }

                // --- Phase 1: try a verified LOCAL checkpoint before any network
                // download. Restore from the newest on-disk snapshot whose height
                // a canonical peer confirms (so we never restore our own forked
                // state), falling back to older checkpoints then the remote path.
                // This avoids re-downloading the whole chain and works even when
                // the public RPC is itself unreachable. ---
                let our_h = engine_clone.height();
                for (cp_h, cp_path) in local_snapshots_for_resync.list().into_iter().rev()
                    .filter(|(h, _)| *h <= our_h)
                {
                    let bytes = match std::fs::read(&cp_path) {
                        Ok(b) => b,
                        Err(e) => { warn!(height = cp_h, error = %e, "local checkpoint read failed — skipping"); continue; }
                    };
                    let meta = match solen_consensus::snapshot::read_snapshot_meta(&bytes) {
                        Ok(m) => m,
                        Err(e) => { warn!(height = cp_h, error = %e, "local checkpoint unreadable — skipping"); continue; }
                    };
                    if !checkpoint_is_canonical(&resync_urls, meta.height, &meta.state_root) {
                        info!(height = meta.height, "local checkpoint not confirmed canonical — trying older / remote");
                        continue;
                    }
                    info!(height = meta.height, path = %cp_path.display(),
                          "restoring from verified local checkpoint (no download)");
                    let store = engine_clone.store();
                    let mut store = store.write().unwrap();
                    store.clear().ok();
                    match solen_consensus::snapshot::restore_snapshot(store.as_mut(), &bytes) {
                        Ok(m) => {
                            drop(store);
                            engine_clone.reset_to_height(m.height, m.epoch);
                            resync_ok = true;
                            info!(height = m.height, epoch = m.epoch, "local checkpoint restored — resync complete");
                            break;
                        }
                        Err(e) => warn!(error = %e, "local checkpoint restore failed — falling back to remote"),
                    }
                }
                } // end backward recovery tiers (run only when our tip is forked)

                if resync_ok { return; }

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
                });

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
            // Skip for non-validators (they never produce) and at genesis (height 0).
            //
            // Recovery probe: while latched, exactly ONE validator — chosen
            // deterministically from shared wall-clock time and rotating through the
            // proposer order each PARTITION_PROBE_INTERVAL window — re-attempts
            // production. Every node agrees on who that is, so there is a single
            // probe block per window; the others accept and attest it, it reaches
            // quorum, finalizes, and unlatches the whole network. The earlier design
            // let each node probe on its own timer and pick a backup proposer from
            // its own *local* `stalled_for`, so several nodes emitted competing
            // blocks at once whose attestations split and never reached quorum — a
            // permanent deadlock (2026-06-08). Pending blocks are cleared on
            // partition detection (engine force-finalize loop), so there is no stale
            // block to compete with the prober's across windows.
            let is_partitioned = is_validator
                && engine_clone.height() > 0
                && engine_clone.is_likely_partitioned();
            let mut probe_producer = false;
            if is_partitioned {
                // Clear bans either way so probe + attestation traffic can flow.
                if let Some(ref handle) = net_for_blocks {
                    handle.report_peer(solen_p2p::reputation::ReputationEvent::ClearAllBans);
                }
                if engine_clone.is_partition_probe_proposer() && engine_clone.partition_probe_due() {
                    tracing::warn!("partition detected — recovery probe (this node is the prober this window)");
                    probe_producer = true;
                    // Fall through to production below.
                } else {
                    tracing::warn!("skipping production — partition detected (not the prober this window)");
                    // (Stranded auto-recovery is handled generally earlier in the
                    // loop — covering both latched and block-sync strands — so it
                    // is not duplicated here.)
                    let our_h = engine_clone.height();

                    // Throttled: request sync to try to reconnect / catch up, but
                    // at most once per partition_sync_interval so a latched node
                    // doesn't flood the network ~20×/sec (see declaration above).
                    if last_partition_sync_at.elapsed() >= partition_sync_interval {
                        last_partition_sync_at = std::time::Instant::now();
                        if let Some(ref handle) = net_for_blocks {
                            handle.broadcast(NetworkMessage::SyncRequest {
                                from_height: our_h + 1,
                                to_height: our_h + 10,
                            });
                        }
                    }
                    // Still check for dropped blocks.
                    let _ = engine_clone.take_dropped_block_height();
                    continue;
                }
            }

            let is_proposer = engine_clone.is_next_proposer();
            let is_backup = engine_clone.is_backup_proposer(stalled_for);
            let active_count = engine_clone.active_validator_count();
            let should_propose = active_count <= 1
                || (is_proposer && !already_pending)
                || (!already_pending && is_backup)
                || (probe_producer && !already_pending);

            if !should_propose && stalled_for.as_secs() > 5 {
                tracing::debug!(
                    height = next_height,
                    is_proposer,
                    already_pending,
                    is_backup,
                    active_count,
                    stalled_secs = stalled_for.as_secs(),
                    "waiting for proposer"
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

/// Find the deepest height in `[min_height, tip)` where our local block's state
/// root still matches the canonical chain (per a seed RPC) — the common ancestor
/// to roll back to. Walks down from the tip; the first match (highest height) is
/// returned as `(height, our_root)`. Returns `None` if no seed answers or no
/// height within range agrees (fork deeper than the journal → caller falls back
/// to a snapshot restore).
fn find_common_ancestor(
    urls: &[String],
    engine: &ConsensusEngine,
    min_height: u64,
    tip: u64,
) -> Option<(u64, [u8; 32])> {
    if tip == 0 {
        return None;
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .ok()?;
    let mut h = tip.saturating_sub(1);
    loop {
        if h < min_height {
            break;
        }
        let our_root = match engine.get_block(h) {
            Some(b) => b.header.state_root,
            None => break, // missing local block — can't compare deeper
        };
        let want = hex(&our_root);
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"solen_getBlock","params":[{h}]}}"#
        );
        let mut canonical: Option<String> = None;
        for url in urls {
            if let Ok(resp) = client.post(url.as_str())
                .header("Content-Type", "application/json")
                .body(body.clone())
                .send()
            {
                if let Ok(j) = resp.json::<serde_json::Value>() {
                    if let Some(r) = j["result"]["state_root"].as_str() {
                        canonical = Some(r.to_string());
                        break;
                    }
                }
            }
        }
        match canonical {
            Some(r) if r.eq_ignore_ascii_case(&want) => return Some((h, our_root)),
            Some(_) => {} // still diverged at h — go deeper
            None => return None, // no seed answered — bail to other recovery paths
        }
        if h == min_height {
            break;
        }
        h -= 1;
    }
    None
}

/// Confirm a local checkpoint is on the canonical chain by checking its height's
/// state root against a seed RPC, before we trust it for a local restore — so we
/// never roll back into our own forked state. Returns the verdict from the first
/// seed that answers (reachable + root matches → true; reachable + root differs
/// → false). If no seed answers we can't confirm, so return false and let the
/// caller fall back to the remote download path.
fn checkpoint_is_canonical(urls: &[String], height: u64, expected_root: &[u8; 32]) -> bool {
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    let want = hex(expected_root);
    let body = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"solen_getBlock","params":[{height}]}}"#
    );
    for url in urls {
        if let Ok(resp) = client.post(url.as_str())
            .header("Content-Type", "application/json")
            .body(body.clone())
            .send()
        {
            if let Ok(json) = resp.json::<serde_json::Value>() {
                if let Some(root) = json["result"]["state_root"].as_str() {
                    // First seed with a definitive answer decides.
                    return root.eq_ignore_ascii_case(&want);
                }
            }
        }
        // This seed didn't answer — try the next.
    }
    false
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
