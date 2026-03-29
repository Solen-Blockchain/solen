//! Combined libp2p network behaviour: gossipsub + Kademlia DHT + identify + mDNS.

use libp2p::{gossipsub, identify, kad, mdns, swarm::NetworkBehaviour};

/// Combined network behaviour for Solen nodes.
#[derive(NetworkBehaviour)]
pub struct SolenBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    pub identify: identify::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
}
