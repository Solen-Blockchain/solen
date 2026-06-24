//! Slashing conditions and evidence processing.

use solen_types::block::BlockHeader;
use solen_types::ValidatorId;
use tracing::warn;

use crate::validator::ValidatorSet;

/// Slashing reason and associated penalty (fraction of stake, in basis points).
#[derive(Debug, Clone)]
pub enum SlashingReason {
    /// Signed two different blocks at the same height.
    DoubleSign {
        height: u64,
        block_a: [u8; 32],
        block_b: [u8; 32],
    },
    /// Missed too many consecutive blocks.
    Downtime { missed_blocks: u64 },
    /// Proposed a block with an invalid state root.
    InvalidStateRoot {
        height: u64,
        expected: [u8; 32],
        got: [u8; 32],
    },
}

impl SlashingReason {
    /// Penalty in basis points (out of 10_000).
    pub fn penalty_bps(&self) -> u64 {
        match self {
            SlashingReason::DoubleSign { .. } => 1000,        // 10% slash
            SlashingReason::Downtime { .. } => 100,           // 1% slash
            SlashingReason::InvalidStateRoot { .. } => 500,   // 5% slash
        }
    }
}

/// Evidence of a slashable offense.
#[derive(Debug, Clone)]
pub struct SlashingEvidence {
    pub offender: ValidatorId,
    pub reason: SlashingReason,
}

/// Downtime threshold: jail after this many consecutive missed proposer slots.
/// With 11 validators at 6s blocks: 100 missed slots ≈ 1,100 blocks ≈ ~1.8 hours.
/// Enough time for a VPS reboot + resync without getting jailed.
pub const DOWNTIME_THRESHOLD: u64 = 100;

/// Check for double-sign: two different block headers signed by the same proposer
/// at the same height. Verifies proposer signatures when present to prevent
/// false slashing from fabricated headers.
pub fn check_double_sign(a: &BlockHeader, b: &BlockHeader) -> Option<SlashingEvidence> {
    if a.height != b.height || a.proposer != b.proposer {
        return None;
    }

    let hash_a = crate::engine::block_hash(a);
    let hash_b = crate::engine::block_hash(b);

    // block_hash() excludes the signature, so two headers with the same hash
    // are the same block (or a re-sign), not equivocation.
    if hash_a == hash_b {
        return None;
    }

    // Two headers that build on the same parent, include the same transactions,
    // and yield the same state are the SAME logical block, merely re-proposed
    // with a fresh wall-clock timestamp/signature — e.g. after the proposer
    // restarted and re-produced a not-yet-finalized height. That is NOT
    // equivocation: nothing forks, the chain is unchanged. Slashing it would
    // punish honest validators for a normal restart AND (because the two hashes
    // split attestations) wedge consensus — the failure the 2026-06-24 devnet
    // drill surfaced. Genuine equivocation differs in parent, transactions, or
    // resulting state — any of which still trips this check.
    if a.parent_hash == b.parent_hash
        && a.transactions_root == b.transactions_root
        && a.state_root == b.state_root
    {
        return None;
    }

    // Real evidence requires BOTH headers to carry a valid 64-byte signature
    // from the proposer. Without this check, anyone could fabricate two
    // unsigned conflicting headers to slash an honest validator — block_hash
    // excludes the signature, so the framing would otherwise look authentic.
    if a.proposer_signature.len() != 64 || b.proposer_signature.len() != 64 {
        return None;
    }
    let mut sig_a = [0u8; 64];
    sig_a.copy_from_slice(&a.proposer_signature);
    if solen_crypto::verify(&a.proposer, &hash_a, &sig_a).is_err() {
        return None; // signature A invalid — not real evidence
    }
    let mut sig_b = [0u8; 64];
    sig_b.copy_from_slice(&b.proposer_signature);
    if solen_crypto::verify(&b.proposer, &hash_b, &sig_b).is_err() {
        return None; // signature B invalid — not real evidence
    }

    Some(SlashingEvidence {
        offender: a.proposer,
        reason: SlashingReason::DoubleSign {
            height: a.height,
            block_a: a.state_root,
            block_b: b.state_root,
        },
    })
}

/// Result of processing slashing evidence.
#[derive(Debug, Clone)]
pub struct SlashingResult {
    pub offender: ValidatorId,
    pub penalty: u128,
    pub remaining_stake: u128,
    pub reason: String,
}

/// Process slashing evidence: apply penalty, jail the offender,
/// and return a record for persistence.
pub fn process_slashing(
    validator_set: &mut ValidatorSet,
    evidence: &SlashingEvidence,
) -> Option<SlashingResult> {
    let penalty_bps = evidence.reason.penalty_bps();

    let result = if let Some(v) = validator_set.get_mut(&evidence.offender) {
        let penalty = v.stake * penalty_bps as u128 / 10_000;
        v.stake = v.stake.saturating_sub(penalty);
        let remaining = v.stake;

        warn!(
            offender = ?evidence.offender[..4],
            penalty,
            remaining_stake = remaining,
            "validator slashed and jailed"
        );

        Some(SlashingResult {
            offender: evidence.offender,
            penalty,
            remaining_stake: remaining,
            reason: format!("{:?}", evidence.reason),
        })
    } else {
        None
    };

    validator_set.jail(&evidence.offender);
    result
}

/// Persist slashing evidence to the state store for audit trail.
pub fn persist_slashing_evidence(
    store: &mut dyn solen_storage::StateStore,
    result: &SlashingResult,
    height: u64,
) {
    let key = format!("slash/{}/{}", hex_encode(&result.offender), height);
    let value = format!(
        "penalty={},remaining={},reason={}",
        result.penalty, result.remaining_stake, result.reason
    );
    let _ = store.put(key.as_bytes(), value.as_bytes());
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Record a missed block for the proposer. Returns slashing evidence if threshold hit.
pub fn record_missed_block(
    validator_set: &mut ValidatorSet,
    proposer: &ValidatorId,
) -> Option<SlashingEvidence> {
    if let Some(v) = validator_set.get_mut(proposer) {
        v.missed_blocks += 1;
        if v.missed_blocks >= DOWNTIME_THRESHOLD {
            return Some(SlashingEvidence {
                offender: *proposer,
                reason: SlashingReason::Downtime {
                    missed_blocks: v.missed_blocks,
                },
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validator::{ValidatorInfo, ValidatorStatus};

    fn vid(n: u8) -> ValidatorId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn double_sign_detected() {
        // Two conflicting headers at the same height, each VALIDLY signed by the
        // same proposer, constitute slashable equivocation.
        let kp = solen_crypto::Keypair::from_seed(&[0x07; 32]);
        let proposer = kp.public_key();

        let mut header_a = BlockHeader {
            height: 10,
            epoch: 0,
            parent_hash: [0; 32],
            state_root: [1; 32],
            transactions_root: [0; 32],
            receipts_root: [0; 32],
            proposer,
            timestamp_ms: 0,
            proposer_signature: vec![],
        };
        let mut header_b = BlockHeader {
            state_root: [2; 32], // different state root → different block
            ..header_a.clone()
        };
        header_a.proposer_signature =
            kp.sign(&crate::engine::block_hash(&header_a)).to_vec();
        header_b.proposer_signature =
            kp.sign(&crate::engine::block_hash(&header_b)).to_vec();

        let evidence = check_double_sign(&header_a, &header_b);
        assert!(evidence.is_some());
        assert_eq!(evidence.unwrap().offender, proposer);

        // Unsigned headers must NOT yield evidence: block_hash() excludes the
        // signature, so accepting unsigned "equivocation" would let anyone
        // fabricate two headers to slash an honest validator.
        let mut unsigned_a = header_a.clone();
        unsigned_a.proposer_signature = vec![];
        let mut unsigned_b = header_b.clone();
        unsigned_b.proposer_signature = vec![];
        assert!(check_double_sign(&unsigned_a, &unsigned_b).is_none());
    }

    #[test]
    fn slash_reduces_stake_and_jails() {
        let mut vs = ValidatorSet::new(vec![
            ValidatorInfo::new(vid(1), 1000),
            ValidatorInfo::new(vid(2), 1000),
            ValidatorInfo::new(vid(3), 1000),
        ]);

        let evidence = SlashingEvidence {
            offender: vid(1),
            reason: SlashingReason::DoubleSign {
                height: 5,
                block_a: [1; 32],
                block_b: [2; 32],
            },
        };

        let result = process_slashing(&mut vs, &evidence);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.penalty, 100); // 10% of 1000
        assert_eq!(result.remaining_stake, 900);

        let v1 = vs.get_mut(&vid(1)).unwrap();
        assert_eq!(v1.stake, 900);
        assert_eq!(v1.status, ValidatorStatus::Jailed);
    }

    #[test]
    fn downtime_triggers_after_threshold() {
        let mut vs = ValidatorSet::new(vec![ValidatorInfo::new(vid(1), 1000)]);

        for _ in 0..DOWNTIME_THRESHOLD - 1 {
            assert!(record_missed_block(&mut vs, &vid(1)).is_none());
        }

        let evidence = record_missed_block(&mut vs, &vid(1));
        assert!(evidence.is_some());
    }
}
