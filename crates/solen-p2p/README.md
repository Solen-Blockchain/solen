# solen-p2p

Peer-to-peer networking for Solen nodes using libp2p.

## Features

- **Gossipsub** — message dissemination across three topics: blocks, transactions, attestations
- **mDNS** — automatic local peer discovery
- **NetworkHandle** — broadcast interface for other subsystems

## Gossip Topics

| Topic | Content |
|-------|---------|
| `solen/blocks/1` | New block announcements (header + metadata) |
| `solen/transactions/1` | User operations for mempool relay |
| `solen/attestations/1` | Validator attestations for finality |

## Usage

```rust
use solen_p2p::network::{NetworkService, NetworkConfig};

let (handle, mut inbound_rx, _task) = NetworkService::start(NetworkConfig {
    listen_port: 30333,
    bootstrap_peers: vec!["/ip4/127.0.0.1/tcp/30334".parse().unwrap()],
}).await?;

// Broadcast a block
handle.broadcast(NetworkMessage::NewBlock { header, tx_count, gas_used });

// Receive messages
while let Some(msg) = inbound_rx.recv().await {
    match msg {
        NetworkMessage::NewTransaction(op) => { /* add to mempool */ }
        NetworkMessage::NewBlock { header, .. } => { /* process */ }
        NetworkMessage::Attestation { .. } => { /* collect */ }
    }
}
```
