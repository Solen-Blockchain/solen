//! Combined libp2p network behaviour: gossipsub + mDNS peer discovery.

use libp2p::{gossipsub, mdns, swarm::NetworkBehaviour};

/// Combined network behaviour for Solen nodes.
#[derive(NetworkBehaviour)]
pub struct SolenBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
}
