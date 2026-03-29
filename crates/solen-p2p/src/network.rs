//! Network service: manages libp2p swarm, gossip subscriptions, and message handling.

use std::time::Duration;

use futures::StreamExt;
use libp2p::gossipsub::{self, IdentTopic, MessageAuthenticity};
use libp2p::identity::Keypair;
use libp2p::swarm::SwarmEvent;
use libp2p::{mdns, noise, tcp, yamux, Multiaddr, SwarmBuilder};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::behaviour::{SolenBehaviour, SolenBehaviourEvent};
use crate::messages::{NetworkMessage, TOPIC_ATTESTATIONS, TOPIC_BLOCKS, TOPIC_TRANSACTIONS};

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
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_port: 30333,
            bootstrap_peers: Vec::new(),
        }
    }
}

/// Handle for sending messages to the network from other subsystems.
#[derive(Clone)]
pub struct NetworkHandle {
    outbound_tx: mpsc::UnboundedSender<NetworkMessage>,
}

impl NetworkHandle {
    /// Broadcast a message to the gossip network.
    pub fn broadcast(&self, msg: NetworkMessage) -> bool {
        self.outbound_tx.send(msg).is_ok()
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
            mpsc::UnboundedReceiver<NetworkMessage>,
            tokio::task::JoinHandle<()>,
        ),
        NetworkError,
    > {
        let local_key = Keypair::generate_ed25519();
        let local_peer_id = local_key.public().to_peer_id();

        info!(%local_peer_id, "starting P2P network");

        // Build gossipsub.
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(Duration::from_secs(1))
            .validation_mode(gossipsub::ValidationMode::Permissive)
            .build()
            .map_err(|e| NetworkError::Gossipsub(e.to_string()))?;

        let gossipsub = gossipsub::Behaviour::new(
            MessageAuthenticity::Signed(local_key.clone()),
            gossipsub_config,
        )
        .map_err(|e| NetworkError::Gossipsub(e.to_string()))?;

        let mdns = mdns::tokio::Behaviour::new(mdns::Config::default(), local_peer_id)
            .map_err(|e| NetworkError::Transport(e.to_string()))?;

        let behaviour = SolenBehaviour { gossipsub, mdns };

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

        // Subscribe to topics.
        let topics = [TOPIC_BLOCKS, TOPIC_TRANSACTIONS, TOPIC_ATTESTATIONS];
        for topic_name in &topics {
            let topic = IdentTopic::new(*topic_name);
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

        // Dial bootstrap peers.
        for addr in &config.bootstrap_peers {
            if let Err(e) = swarm.dial(addr.clone()) {
                warn!(%addr, error = %e, "failed to dial bootstrap peer");
            }
        }

        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<NetworkMessage>();
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<NetworkMessage>();

        let handle = NetworkHandle { outbound_tx };

        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Handle outbound messages.
                    Some(msg) = outbound_rx.recv() => {
                        let topic = IdentTopic::new(msg.topic());
                        let data = msg.encode();
                        if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                            debug!(error = %e, "failed to publish message");
                        }
                    }
                    // Handle swarm events.
                    event = swarm.select_next_some() => {
                        match event {
                            SwarmEvent::Behaviour(SolenBehaviourEvent::Gossipsub(
                                gossipsub::Event::Message { message, .. },
                            )) => {
                                if let Ok(msg) = NetworkMessage::decode(&message.data) {
                                    let _ = inbound_tx.send(msg);
                                }
                            }
                            SwarmEvent::Behaviour(SolenBehaviourEvent::Mdns(
                                mdns::Event::Discovered(peers),
                            )) => {
                                for (peer_id, addr) in peers {
                                    debug!(%peer_id, %addr, "discovered peer via mDNS");
                                    swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                                }
                            }
                            SwarmEvent::Behaviour(SolenBehaviourEvent::Mdns(
                                mdns::Event::Expired(peers),
                            )) => {
                                for (peer_id, _) in peers {
                                    swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                                }
                            }
                            SwarmEvent::NewListenAddr { address, .. } => {
                                info!(%address, "listening on");
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
            },
            operations: vec![],
            tx_count: 5,
            gas_used: 1000,
        };

        let encoded = msg.encode();
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

        let encoded = msg.encode();
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
