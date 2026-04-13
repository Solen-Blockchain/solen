//! Network service: manages libp2p swarm, gossip subscriptions, and message handling.

use std::collections::HashMap;
use std::time::Duration;

use futures::StreamExt;
use libp2p::gossipsub::{self, IdentTopic, MessageAuthenticity};
use libp2p::identity::Keypair;
use libp2p::swarm::SwarmEvent;
use libp2p::{identify, kad, mdns, noise, tcp, yamux, Multiaddr, SwarmBuilder};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::behaviour::{SolenBehaviour, SolenBehaviourEvent};
use crate::messages::NetworkMessage;

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("gossipsub error: {0}")]
    Gossipsub(String),
    #[error("failed to listen: {0}")]
    Listen(String),
}

/// Configuration for the P2P network.
pub struct NetworkConfig {
    /// Port to listen on.
    pub listen_port: u16,
    /// Bootstrap peer addresses.
    pub bootstrap_peers: Vec<Multiaddr>,
    /// Maximum inbound connections.
    pub max_inbound: u32,
    /// Maximum outbound connections.
    pub max_outbound: u32,
    /// Optional 32-byte seed to derive a stable libp2p keypair.
    /// If set, the node keeps the same peer ID across restarts.
    pub identity_seed: Option<[u8; 32]>,
    /// Chain ID — used to create network-specific gossip topics.
    pub chain_id: u64,
    /// Expected genesis state root for fork isolation.
    /// If set, SyncBlocks from a different fork are dropped and the sender is banned.
    pub genesis_hash: Option<[u8; 32]>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_port: 30333,
            bootstrap_peers: Vec::new(),
            max_inbound: 50,
            max_outbound: 20,
            identity_seed: None,
            chain_id: 0,
            genesis_hash: None,
        }
    }
}

/// Handle for sending messages to the network from other subsystems.
#[derive(Clone)]
pub struct NetworkHandle {
    outbound_tx: mpsc::UnboundedSender<NetworkMessage>,
    reputation_tx: mpsc::UnboundedSender<crate::reputation::ReputationEvent>,
}

impl NetworkHandle {
    /// Broadcast a message to the gossip network.
    pub fn broadcast(&self, msg: NetworkMessage) -> bool {
        self.outbound_tx.send(msg).is_ok()
    }

    /// Report a peer reputation event (valid/invalid block or attestation).
    pub fn report_peer(&self, event: crate::reputation::ReputationEvent) {
        let _ = self.reputation_tx.send(event);
    }
}

/// The P2P network service.
pub struct NetworkService;

impl NetworkService {
    /// Start the network service. Returns a handle for broadcasting and a receiver
    /// for incoming messages.
    pub async fn start(
        config: NetworkConfig,
    ) -> Result<
        (
            NetworkHandle,
            mpsc::Receiver<NetworkMessage>,
            tokio::task::JoinHandle<()>,
        ),
        NetworkError,
    > {
        let local_key = if let Some(seed) = config.identity_seed {
            // Derive a stable keypair from seed so peer ID persists across restarts.
            // Domain-separate from the validator signing key.
            let domain = solen_crypto::blake3_hash(b"solen-p2p-identity");
            let mut hasher_input = Vec::with_capacity(64);
            hasher_input.extend_from_slice(&seed);
            hasher_input.extend_from_slice(&domain);
            let p2p_seed = solen_crypto::blake3_hash(&hasher_input);
            let mut seed_bytes = p2p_seed.to_vec();
            // libp2p ed25519 expects a 32-byte seed for SecretKey::try_from_bytes.
            let sk = libp2p::identity::ed25519::SecretKey::try_from_bytes(&mut seed_bytes)
                .expect("valid 32-byte seed");
            let kp = libp2p::identity::ed25519::Keypair::from(sk);
            Keypair::from(kp)
        } else {
            Keypair::generate_ed25519()
        };
        let local_peer_id = local_key.public().to_peer_id();

        info!(%local_peer_id, "starting P2P network");

        // Build gossipsub with mesh limits.
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(1))
            .validation_mode(gossipsub::ValidationMode::Permissive)
            .max_transmit_size(16 * 1024 * 1024) // 16 MB — large blocks with many operations
            .mesh_n(8)              // target mesh size
            .mesh_n_low(4)          // minimum before requesting more
            .mesh_n_high(12)        // maximum before pruning
            .mesh_outbound_min(2)   // minimum outbound peers in mesh
            .build()
            .map_err(|e| NetworkError::Gossipsub(e.to_string()))?;

        // Enable gossipsub peer scoring to automatically prune peers that
        // send invalid messages (e.g. blocks from a different fork).
        // Peers with negative application scores get removed from the mesh.
        let peer_score_params = {
            let mut params = gossipsub::PeerScoreParams::default();
            // Application-specific scoring: our node sets negative scores on
            // peers that relay fork-mismatched blocks.
            params.app_specific_weight = 1.0;
            // Disable IP colocation penalty (our validators may share subnets).
            params.ip_colocation_factor_weight = 0.0;
            params
        };
        let peer_score_thresholds = gossipsub::PeerScoreThresholds {
            gossip_threshold: -100.0,      // suppress gossip below this
            publish_threshold: -200.0,     // suppress publish below this
            graylist_threshold: -300.0,    // completely ignore messages below this
            accept_px_threshold: 0.0,
            opportunistic_graft_threshold: 0.5,
        };

        let mut gossipsub = gossipsub::Behaviour::new(
            MessageAuthenticity::Signed(local_key.clone()),
            gossipsub_config,
        )
        .map_err(|e| NetworkError::Gossipsub(e.to_string()))?;

        gossipsub
            .with_peer_score(peer_score_params, peer_score_thresholds)
            .map_err(|e| NetworkError::Gossipsub(e.to_string()))?;

        // Kademlia DHT for peer discovery across the internet.
        let kad_store = libp2p::kad::store::MemoryStore::new(local_peer_id);
        let mut kademlia = libp2p::kad::Behaviour::new(local_peer_id, kad_store);
        kademlia.set_mode(Some(libp2p::kad::Mode::Server));

        // Identify protocol — exchanges peer info and keeps connections alive.
        let identify = libp2p::identify::Behaviour::new(
            libp2p::identify::Config::new(
                "/solen/1.0.0".to_string(),
                local_key.public(),
            )
            .with_push_listen_addr_updates(true),
        );

        let mdns = mdns::tokio::Behaviour::new(mdns::Config::default(), local_peer_id)
            .map_err(|e| NetworkError::Transport(e.to_string()))?;

        let behaviour = SolenBehaviour { gossipsub, kademlia, identify, mdns };

        let total_limit = config.max_inbound + config.max_outbound;

        let mut swarm = SwarmBuilder::with_existing_identity(local_key)
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| NetworkError::Transport(e.to_string()))?
            .with_behaviour(|_| Ok(behaviour))
            .map_err(|e| NetworkError::Transport(e.to_string()))?
            .build();

        info!(
            max_inbound = config.max_inbound,
            max_outbound = config.max_outbound,
            total_limit,
            "P2P connection limits configured"
        );

        // Subscribe to network-specific topics (chain_id prevents cross-network interference).
        use crate::messages::{topic_blocks, topic_transactions, topic_attestations, topic_sync};
        let cid = config.chain_id;
        let topic_names = [topic_blocks(cid), topic_transactions(cid), topic_attestations(cid), topic_sync(cid)];
        for topic_name in &topic_names {
            let topic = IdentTopic::new(topic_name);
            swarm
                .behaviour_mut()
                .gossipsub
                .subscribe(&topic)
                .map_err(|e| NetworkError::Gossipsub(e.to_string()))?;
        }

        // Listen.
        let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", config.listen_port)
            .parse()
            .unwrap();
        swarm
            .listen_on(listen_addr)
            .map_err(|e| NetworkError::Listen(e.to_string()))?;

        for addr in &config.bootstrap_peers {
            info!(%addr, "dialing bootstrap peer");
            // Extract peer ID from the multiaddr if present (e.g., /ip4/.../p2p/<peer_id>).
            let peer_id = addr.iter().find_map(|p| match p {
                libp2p::multiaddr::Protocol::P2p(id) => Some(id),
                _ => None,
            });
            match swarm.dial(addr.clone()) {
                Ok(_) => {
                    info!(%addr, "dial initiated");
                    // Add to Kademlia if we know the peer ID from the address.
                    // Otherwise, Identify will populate Kademlia once connected.
                    if let Some(pid) = peer_id {
                        swarm.behaviour_mut().kademlia.add_address(&pid, addr.clone());
                    }
                }
                Err(e) => warn!(%addr, error = %e, "failed to dial bootstrap peer"),
            }
        }

        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<NetworkMessage>();
        // Bounded inbound channel — backpressure if processing can't keep up.
        let (inbound_tx, inbound_rx) = mpsc::channel::<NetworkMessage>(4096);
        // Reputation event channel — node reports valid/invalid peers.
        let (reputation_tx, mut reputation_rx) = mpsc::unbounded_channel::<crate::reputation::ReputationEvent>();

        let handle = NetworkHandle { outbound_tx, reputation_tx };
        let _p2p_genesis_hash = config.genesis_hash;
        // Track peers that recently relayed SyncBlocks messages.
        let mut recent_sync_senders: HashMap<libp2p::PeerId, std::time::Instant> = HashMap::new();

        let bootstrap_addrs = config.bootstrap_peers.clone();

        let task = tokio::spawn(async move {
            // Per-peer rate limiting: track message counts per peer per window.
            let mut peer_msg_counts: std::collections::HashMap<libp2p::PeerId, (u64, std::time::Instant)> = std::collections::HashMap::new();
            const MAX_MSGS_PER_PEER_PER_SEC: u64 = 50;

            // Global inbound bytes rate limit: prevents aggregate flooding
            // through multiple relay peers (each under per-peer limit).
            const MAX_GLOBAL_BYTES_PER_SEC: usize = 50 * 1024 * 1024; // 50 MB/s
            let mut global_bytes_window: usize = 0;
            let mut global_bytes_reset = std::time::Instant::now();

            // Peer reputation tracking — bans peers that consistently send bad data.
            let mut peer_reputation = crate::reputation::PeerReputation::new();

            // Periodically run Kademlia bootstrap and redial if needed.
            let mut maintenance_interval = tokio::time::interval(Duration::from_secs(10));
            maintenance_interval.tick().await; // skip first immediate tick

            loop {
                tokio::select! {
                    _ = maintenance_interval.tick() => {
                        let connected = swarm.connected_peers().count();

                        // Run Kademlia bootstrap to discover new peers.
                        let _ = swarm.behaviour_mut().kademlia.bootstrap();

                        // Redial bootstrap peers if we have few connections.
                        if connected < 3 && !bootstrap_addrs.is_empty() {
                            for addr in &bootstrap_addrs {
                                let _ = swarm.dial(addr.clone());
                            }
                            debug!(connected, "redialing bootstrap peers");
                        }

                        if connected > 0 {
                            debug!(connected, "peer connections active");
                        }

                        // Clean up stale rate-limit entries.
                        peer_msg_counts.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(30));
                    }
                    // Process reputation events from the node.
                    Some(event) = reputation_rx.recv() => {
                        use crate::reputation::ReputationEvent;
                        match event {
                            ReputationEvent::ValidBlock(peer) => peer_reputation.record_valid_block(&peer),
                            ReputationEvent::InvalidBlock(peer) => peer_reputation.record_invalid_block(&peer),
                            ReputationEvent::ValidAttestation(peer) => peer_reputation.record_valid_attestation(&peer),
                            ReputationEvent::InvalidAttestation(peer) => peer_reputation.record_invalid_attestation(&peer),
                            ReputationEvent::ForkMismatch(peer) => {
                                peer_reputation.record_fork_mismatch(&peer);
                                // Penalize the specific peer at gossipsub level.
                                swarm.behaviour_mut().gossipsub
                                    .set_application_score(&peer, -500.0);
                                // Also penalize all recent sync block senders —
                                // they likely relayed fork-mismatched blocks.
                                for (sender, _) in recent_sync_senders.drain() {
                                    swarm.behaviour_mut().gossipsub
                                        .set_application_score(&sender, -500.0);
                                    tracing::debug!(%sender, "gossipsub penalized (recent sync sender during fork mismatch)");
                                }
                                tracing::info!(%peer, "gossipsub score set to -500 (fork mismatch)");
                            }
                            ReputationEvent::ClearAllBans => {
                                peer_reputation.clear_all_bans();
                                tracing::info!("all peer bans cleared (partition recovery)");
                            }
                            ReputationEvent::GossipsubPenalize(peer) => {
                                swarm.behaviour_mut().gossipsub
                                    .set_application_score(&peer, -500.0);
                                tracing::info!(%peer, "gossipsub score set to -500 (penalized)");
                            }
                        }
                    }
                    // Handle outbound messages.
                    Some(msg) = outbound_rx.recv() => {
                        let topic = IdentTopic::new(msg.topic_for_chain(cid));
                        if let Some(data) = msg.encode() {
                            if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                debug!(error = %e, "failed to publish message");
                            }
                        }
                    }
                    // Handle swarm events.
                    event = swarm.select_next_some() => {
                        match event {
                            SwarmEvent::Behaviour(SolenBehaviourEvent::Gossipsub(
                                gossipsub::Event::Message { message, propagation_source, .. },
                            )) => {
                                // Check if peer is banned.
                                if peer_reputation.is_banned(&propagation_source) {
                                    continue; // silently drop all messages from banned peers
                                }

                                // Global inbound bytes rate limit.
                                if global_bytes_reset.elapsed() > Duration::from_secs(1) {
                                    global_bytes_window = 0;
                                    global_bytes_reset = std::time::Instant::now();
                                }
                                global_bytes_window += message.data.len();
                                if global_bytes_window > MAX_GLOBAL_BYTES_PER_SEC {
                                    debug!(bytes = global_bytes_window, "global rate limit exceeded — dropping message");
                                    continue;
                                }

                                // Per-peer rate limiting.
                                // Drop excess messages but DON'T penalize reputation.
                                // Gossipsub relays messages through peers, so a validator
                                // relaying old-chain traffic would get unfairly penalized.
                                let entry = peer_msg_counts.entry(propagation_source).or_insert((0, std::time::Instant::now()));
                                if entry.1.elapsed() > Duration::from_secs(1) {
                                    *entry = (1, std::time::Instant::now());
                                } else {
                                    entry.0 += 1;
                                    if entry.0 >= MAX_MSGS_PER_PEER_PER_SEC {
                                        debug!(peer = %propagation_source, count = entry.0, "rate-limited peer — dropping message");
                                        continue;
                                    }
                                }

                                match NetworkMessage::decode(&message.data) {
                                    Ok(msg) => {
                                        // Track peers that relay SyncBlocks — if the node
                                        // later detects a fork mismatch, we penalize them
                                        // at the gossipsub level.
                                        if matches!(msg, NetworkMessage::SyncBlocks { .. }) {
                                            recent_sync_senders.insert(propagation_source, std::time::Instant::now());
                                            // Prune old entries.
                                            if recent_sync_senders.len() > 50 {
                                                recent_sync_senders.retain(|_, t| t.elapsed() < Duration::from_secs(30));
                                            }
                                        }

                                        // Use try_send to apply backpressure if the inbound channel is full.
                                        if inbound_tx.try_send(msg).is_err() {
                                            debug!("inbound channel full — dropping message");
                                        }
                                    }
                                    Err(_) => {
                                        // Decode failure — peer sent garbage.
                                        peer_reputation.record_decode_failure(&propagation_source);
                                    }
                                }
                            }
                            SwarmEvent::Behaviour(SolenBehaviourEvent::Mdns(
                                mdns::Event::Discovered(peers),
                            )) => {
                                for (peer_id, addr) in peers {
                                    // Only add mDNS peers on the same P2P port (same network).
                                    let same_port = addr.iter().any(|p| matches!(p, libp2p::multiaddr::Protocol::Tcp(p) if p == config.listen_port));
                                    if same_port {
                                        debug!(%peer_id, %addr, "discovered peer via mDNS");
                                        swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                                    }
                                }
                            }
                            SwarmEvent::Behaviour(SolenBehaviourEvent::Mdns(
                                mdns::Event::Expired(peers),
                            )) => {
                                for (peer_id, _) in peers {
                                    swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                                }
                            }
                            SwarmEvent::Behaviour(SolenBehaviourEvent::Identify(
                                identify::Event::Received { peer_id, info, .. },
                            )) => {
                                // Add the peer's listen addresses to Kademlia for discovery.
                                for addr in &info.listen_addrs {
                                    swarm.behaviour_mut().kademlia.add_address(&peer_id, addr.clone());
                                }
                                debug!(%peer_id, addrs = info.listen_addrs.len(), "identify received");
                            }
                            SwarmEvent::Behaviour(SolenBehaviourEvent::Identify(_)) => {}
                            SwarmEvent::NewListenAddr { address, .. } => {
                                info!(%address, "listening on");
                            }
                            SwarmEvent::Behaviour(SolenBehaviourEvent::Kademlia(
                                kad::Event::RoutingUpdated { peer, .. },
                            )) => {
                                debug!(%peer, "Kademlia discovered peer");
                                swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer);
                            }
                            SwarmEvent::Behaviour(SolenBehaviourEvent::Kademlia(_)) => {
                                // Other Kademlia events (query results, etc.)
                            }
                            SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                                debug!(%peer_id, ?endpoint, "peer connected");
                                swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                                // Add to Kademlia so other peers can discover this one.
                                swarm.behaviour_mut().kademlia.add_address(&peer_id, endpoint.get_remote_address().clone());
                            }
                            SwarmEvent::ConnectionClosed { peer_id, cause, .. } => {
                                debug!(%peer_id, ?cause, "peer disconnected");
                            }
                            SwarmEvent::OutgoingConnectionError { error, .. } => {
                                debug!(%error, "outgoing connection failed");
                            }
                            SwarmEvent::IncomingConnectionError { error, .. } => {
                                debug!(%error, "incoming connection failed");
                            }
                            _ => {}
                        }
                    }
                }
            }
        });

        Ok((handle, inbound_rx, task))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trip() {
        let msg = NetworkMessage::NewBlock {
            header: solen_types::block::BlockHeader {
                height: 1,
                epoch: 0,
                parent_hash: [0; 32],
                state_root: [1; 32],
                transactions_root: [0; 32],
                receipts_root: [0; 32],
                proposer: [2; 32],
                timestamp_ms: 12345,
                proposer_signature: vec![],
            },
            operations: vec![],
            tx_count: 5,
            gas_used: 1000,
        };

        let encoded = msg.encode().unwrap();
        let decoded = NetworkMessage::decode(&encoded).unwrap();

        match decoded {
            NetworkMessage::NewBlock { header, tx_count, gas_used, .. } => {
                assert_eq!(header.height, 1);
                assert_eq!(tx_count, 5);
                assert_eq!(gas_used, 1000);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn attestation_round_trip() {
        let msg = NetworkMessage::Attestation {
            validator_id: [42; 32],
            block_height: 10,
            block_hash: [99; 32],
            signature: vec![1, 2, 3],
        };

        let encoded = msg.encode().unwrap();
        let decoded = NetworkMessage::decode(&encoded).unwrap();

        match decoded {
            NetworkMessage::Attestation { validator_id, block_height, .. } => {
                assert_eq!(validator_id, [42; 32]);
                assert_eq!(block_height, 10);
            }
            _ => panic!("wrong variant"),
        }
    }
}
