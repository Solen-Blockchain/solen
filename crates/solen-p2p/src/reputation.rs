//! Peer reputation tracking.
//!
//! Tracks peer behavior to identify and temporarily ban misbehaving peers.
//! Peers accumulate positive score for valid messages and negative score for
//! invalid ones. Below a threshold, the peer is banned for a cooldown period.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use libp2p::PeerId;

/// Events sent from the node to the P2P layer to update peer reputation.
#[derive(Debug, Clone)]
pub enum ReputationEvent {
    ValidBlock(PeerId),
    InvalidBlock(PeerId),
    ValidAttestation(PeerId),
    InvalidAttestation(PeerId),
}

/// Score thresholds and timing.
const BAN_THRESHOLD: i64 = -50;
const BAN_DURATION_SECS: u64 = 300; // 5 minutes
const DECAY_INTERVAL_SECS: u64 = 60;
const DECAY_AMOUNT: i64 = 5; // score decays toward 0 every interval
const MAX_TRACKED_PEERS: usize = 500;

/// Score changes for different events.
const VALID_BLOCK_SCORE: i64 = 2;
const VALID_ATTESTATION_SCORE: i64 = 1;
const INVALID_BLOCK_SCORE: i64 = -10;
const INVALID_ATTESTATION_SCORE: i64 = -5;
const DECODE_FAILURE_SCORE: i64 = -3;
const RATE_LIMITED_SCORE: i64 = -1;

/// Per-peer reputation state.
#[derive(Debug, Clone)]
struct PeerScore {
    score: i64,
    last_decay: Instant,
    banned_until: Option<Instant>,
    valid_messages: u64,
    invalid_messages: u64,
}

impl PeerScore {
    fn new() -> Self {
        Self {
            score: 0,
            last_decay: Instant::now(),
            banned_until: None,
            valid_messages: 0,
            invalid_messages: 0,
        }
    }

    fn apply_decay(&mut self) {
        if self.last_decay.elapsed() > Duration::from_secs(DECAY_INTERVAL_SECS) {
            if self.score < 0 {
                self.score = (self.score + DECAY_AMOUNT).min(0);
            } else if self.score > 0 {
                self.score = (self.score - DECAY_AMOUNT).max(0);
            }
            self.last_decay = Instant::now();
        }
    }

    fn adjust(&mut self, delta: i64) {
        self.apply_decay();
        self.score += delta;

        if delta > 0 {
            self.valid_messages += 1;
        } else {
            self.invalid_messages += 1;
        }

        // Check ban threshold.
        if self.score <= BAN_THRESHOLD && self.banned_until.is_none() {
            self.banned_until = Some(Instant::now() + Duration::from_secs(BAN_DURATION_SECS));
            tracing::warn!(
                score = self.score,
                ban_secs = BAN_DURATION_SECS,
                "peer banned due to low reputation"
            );
        }
    }

    fn is_banned(&mut self) -> bool {
        if let Some(until) = self.banned_until {
            if Instant::now() >= until {
                // Ban expired — reset.
                self.banned_until = None;
                self.score = 0;
                false
            } else {
                true
            }
        } else {
            false
        }
    }
}

/// Tracks reputation for all known peers.
pub struct PeerReputation {
    peers: HashMap<PeerId, PeerScore>,
}

impl PeerReputation {
    pub fn new() -> Self {
        Self {
            peers: HashMap::new(),
        }
    }

    fn get_or_create(&mut self, peer: &PeerId) -> &mut PeerScore {
        // Prune if too many tracked peers.
        if self.peers.len() > MAX_TRACKED_PEERS {
            let now = Instant::now();
            self.peers.retain(|_, s| {
                s.last_decay.elapsed() < Duration::from_secs(600) || s.banned_until.is_some()
            });
        }
        self.peers.entry(*peer).or_insert_with(PeerScore::new)
    }

    /// Check if a peer is currently banned.
    pub fn is_banned(&mut self, peer: &PeerId) -> bool {
        self.get_or_create(peer).is_banned()
    }

    /// Record a valid block received from this peer.
    pub fn record_valid_block(&mut self, peer: &PeerId) {
        self.get_or_create(peer).adjust(VALID_BLOCK_SCORE);
    }

    /// Record a valid attestation from this peer.
    pub fn record_valid_attestation(&mut self, peer: &PeerId) {
        self.get_or_create(peer).adjust(VALID_ATTESTATION_SCORE);
    }

    /// Record an invalid block from this peer.
    pub fn record_invalid_block(&mut self, peer: &PeerId) {
        self.get_or_create(peer).adjust(INVALID_BLOCK_SCORE);
    }

    /// Record an invalid attestation from this peer.
    pub fn record_invalid_attestation(&mut self, peer: &PeerId) {
        self.get_or_create(peer).adjust(INVALID_ATTESTATION_SCORE);
    }

    /// Record a message decode failure from this peer.
    pub fn record_decode_failure(&mut self, peer: &PeerId) {
        self.get_or_create(peer).adjust(DECODE_FAILURE_SCORE);
    }

    /// Record that this peer was rate-limited.
    pub fn record_rate_limited(&mut self, peer: &PeerId) {
        self.get_or_create(peer).adjust(RATE_LIMITED_SCORE);
    }

    /// Get the score for a peer (for diagnostics).
    pub fn score(&mut self, peer: &PeerId) -> i64 {
        self.get_or_create(peer).apply_decay();
        self.get_or_create(peer).score
    }

    /// Get summary stats.
    pub fn stats(&self) -> (usize, usize) {
        let total = self.peers.len();
        let banned = self.peers.values()
            .filter(|s| s.banned_until.is_some())
            .count();
        (total, banned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer(n: u8) -> PeerId {
        // Generate a deterministic PeerId for testing.
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        keypair.public().to_peer_id()
    }

    #[test]
    fn valid_messages_increase_score() {
        let mut rep = PeerReputation::new();
        let peer = test_peer(1);

        rep.record_valid_block(&peer);
        rep.record_valid_block(&peer);
        assert!(rep.score(&peer) > 0);
        assert!(!rep.is_banned(&peer));
    }

    #[test]
    fn invalid_messages_decrease_score() {
        let mut rep = PeerReputation::new();
        let peer = test_peer(1);

        for _ in 0..5 {
            rep.record_invalid_block(&peer);
        }
        // 5 * -10 = -50, should be at ban threshold.
        assert!(rep.is_banned(&peer));
    }

    #[test]
    fn mixed_behavior_balances() {
        let mut rep = PeerReputation::new();
        let peer = test_peer(1);

        // 10 valid blocks (+20) then 1 invalid (-10) = +10 net
        for _ in 0..10 {
            rep.record_valid_block(&peer);
        }
        rep.record_invalid_block(&peer);
        assert!(!rep.is_banned(&peer));
        assert!(rep.score(&peer) > 0);
    }

    #[test]
    fn decode_failures_accumulate() {
        let mut rep = PeerReputation::new();
        let peer = test_peer(1);

        // 17 decode failures * -3 = -51, should ban.
        for _ in 0..17 {
            rep.record_decode_failure(&peer);
        }
        assert!(rep.is_banned(&peer));
    }
}
