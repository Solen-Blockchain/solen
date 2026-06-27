//! Combined libp2p network behaviour: gossipsub + Kademlia DHT + identify + mDNS
//! + connection limits.

use libp2p::{connection_limits, gossipsub, identify, kad, mdns, swarm::NetworkBehaviour};

/// Combined network behaviour for Solen nodes.
#[derive(NetworkBehaviour)]
pub struct SolenBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    pub identify: identify::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
    /// H-06: enforce the configured inbound/outbound connection caps. libp2p
    /// refuses connections past the limit before they reach a protocol handler.
    pub connection_limits: connection_limits::Behaviour,
}
