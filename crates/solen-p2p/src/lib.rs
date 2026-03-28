//! Peer-to-peer networking for Solen validators and nodes.
//!
//! Uses libp2p with gossipsub for message dissemination:
//! - Block announcements
//! - Transaction propagation
//! - Attestation broadcasting

pub mod behaviour;
pub mod messages;
pub mod network;
