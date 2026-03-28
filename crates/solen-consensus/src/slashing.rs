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
}

impl SlashingReason {
    /// Penalty in basis points (out of 10_000).
    pub fn penalty_bps(&self) -> u64 {
        match self {
            SlashingReason::DoubleSign { .. } => 1000, // 10% slash
            SlashingReason::Downtime { .. } => 100,    // 1% slash
        }
    }
}

/// Evidence of a slashable offense.
#[derive(Debug, Clone)]
pub struct SlashingEvidence {
    pub offender: ValidatorId,
    pub reason: SlashingReason,
}

/// Downtime threshold: jail after this many consecutive missed blocks.
pub const DOWNTIME_THRESHOLD: u64 = 50;

/// Check for double-sign: two different block headers signed by the same proposer
/// at the same height.
pub fn check_double_sign(a: &BlockHeader, b: &BlockHeader) -> Option<SlashingEvidence> {
    if a.height == b.height && a.proposer == b.proposer && a.state_root != b.state_root {
        Some(SlashingEvidence {
            offender: a.proposer,
            reason: SlashingReason::DoubleSign {
                height: a.height,
                block_a: a.state_root,
                block_b: b.state_root,
            },
        })
    } else {
        None
    }
}

/// Process slashing evidence: apply penalty and jail the offender.
pub fn process_slashing(validator_set: &mut ValidatorSet, evidence: &SlashingEvidence) {
    let penalty_bps = evidence.reason.penalty_bps();

    if let Some(v) = validator_set.get_mut(&evidence.offender) {
        let penalty = v.stake * penalty_bps as u128 / 10_000;
        v.stake = v.stake.saturating_sub(penalty);
        let remaining = v.stake;

        warn!(
            offender = ?evidence.offender[..4],
            penalty,
            remaining_stake = remaining,
            "validator slashed and jailed"
        );
    }
    validator_set.jail(&evidence.offender);
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
        let header_a = BlockHeader {
            height: 10,
            epoch: 0,
            parent_hash: [0; 32],
            state_root: [1; 32],
            transactions_root: [0; 32],
            receipts_root: [0; 32],
            proposer: vid(1),
            timestamp_ms: 0,
        };
        let header_b = BlockHeader {
            height: 10,
            epoch: 0,
            parent_hash: [0; 32],
            state_root: [2; 32], // different state root
            transactions_root: [0; 32],
            receipts_root: [0; 32],
            proposer: vid(1),
            timestamp_ms: 0,
        };

        let evidence = check_double_sign(&header_a, &header_b);
        assert!(evidence.is_some());
        assert_eq!(evidence.unwrap().offender, vid(1));
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

        process_slashing(&mut vs, &evidence);

        let v1 = vs.get_mut(&vid(1)).unwrap();
        assert_eq!(v1.stake, 900); // 10% slashed
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
