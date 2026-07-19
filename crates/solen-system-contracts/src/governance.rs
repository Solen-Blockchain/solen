//! Governance contract: proposals, stake-weighted voting, timelocked execution.
//!
//! Solen favors minimal governance. Only narrow parameter changes are
//! allowed through on-chain governance. Core protocol changes require
//! multi-phase qualification.

use serde::{Deserialize, Serialize};
use solen_types::AccountId;
use thiserror::Error;

/// Default voting period in epochs (used if not set in genesis).
pub const DEFAULT_VOTING_PERIOD: u64 = 14;

/// Upper bound on a sane voting period (epochs). A stored value above this is
/// treated as a misconfiguration: mainnet genesis set `governance_voting_period`
/// to 201,600 — that was "14 days in 6-second blocks", but this field is counted
/// in EPOCHS (100 blocks each), making it ~100× too long (~3.8 years), which
/// would freeze governance because no proposal could ever be finalized. When the
/// stored value exceeds this bound, `effective_voting_period()` substitutes
/// `SANE_VOTING_PERIOD`. ~50,000 epochs ≈ 1 year at mainnet's 10-minute epochs.
pub const MAX_SANE_VOTING_PERIOD: u64 = 50_000;

/// Fallback voting period (epochs) used when the configured one is out of range
/// (0 or absurdly large). 2,016 epochs ≈ 14 days at mainnet's 10-minute epochs —
/// the apparent original intent of the 201,600 genesis value.
pub const SANE_VOTING_PERIOD: u64 = 2_016;

/// Timelock delay after passing (in epochs).
pub const TIMELOCK_DELAY: u64 = 3;

/// Quorum: minimum participation as basis points of total stake.
pub const QUORUM_BPS: u64 = 3000; // 30%

/// Supermajority threshold for passing (basis points).
pub const PASS_THRESHOLD_BPS: u64 = 6667; // 66.67%

/// Activation epoch for the EMERGENCY-action fast-track. A circuit breaker that
/// still needs the full ~2016-epoch voting period + timelock isn't a circuit
/// breaker, so once active, an EmergencyPause/EmergencyResume proposal can be
/// finalized the moment it reaches quorum + supermajority (no voting-window wait)
/// and executed with NO timelock. Normal proposals are unaffected — deliberate
/// governance keeps its window. The 66.67% supermajority still applies to the
/// fast-track (raise the threshold check for emergencies if you want a higher bar).
///
/// CONSENSUS-AFFECTING: it changes WHEN a proposal executes, which is in the state
/// root. Dormant by default (`u64::MAX`); to activate, set a chosen future epoch,
/// rebuild, and roll to ALL nodes before that epoch (coordinated flag-day, like
/// `fork_choice_v2_height`). Until then behaviour is unchanged.
pub const EMERGENCY_FASTTRACK_ACTIVATION_EPOCH: u64 = u64::MAX;

/// Whether the emergency fast-track is active at `epoch`.
pub fn emergency_fasttrack_active(epoch: u64) -> bool {
    epoch >= EMERGENCY_FASTTRACK_ACTIVATION_EPOCH
}

/// Emergency actions (circuit breaker) eligible for the fast-track.
fn is_emergency(action: &ProposalAction) -> bool {
    matches!(action, ProposalAction::EmergencyPause | ProposalAction::EmergencyResume)
}

/// Minimum deposit to create a proposal (in base units).
/// Returned if proposal passes, sent to treasury if rejected.
/// 1,000 SOLEN = 100,000,000,000 base units.
pub const PROPOSAL_DEPOSIT: u128 = 1_000 * 100_000_000;

#[derive(Debug, Error)]
pub enum GovernanceError {
    #[error("proposal not found")]
    ProposalNotFound,
    #[error("voting period ended")]
    VotingEnded,
    #[error("voting period not ended")]
    VotingNotEnded,
    #[error("already voted")]
    AlreadyVoted,
    #[error("proposal not passed")]
    ProposalNotPassed,
    #[error("timelock not expired")]
    TimelockNotExpired,
    #[error("proposal already executed")]
    AlreadyExecuted,
    #[error("invalid proposal type")]
    InvalidProposalType,
}

/// Types of parameter changes that governance can enact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProposalAction {
    /// Change the base fee per gas.
    SetBaseFee { new_fee: u128 },
    /// Change block time.
    SetBlockTime { new_block_time_ms: u64 },
    /// Change maximum operations per block.
    SetMaxOpsPerBlock { new_max: usize },
    /// Register a new rollup domain.
    RegisterRollup { rollup_id: u64, name: String },
    /// Change the fee burn rate (basis points, 0-10000).
    SetBurnRate { new_burn_rate_bps: u64 },
    /// Change epoch reward amount (base units per epoch).
    SetEpochReward { new_reward: u128 },
    /// Change minimum validator self-stake (base units).
    SetMinValidatorStake { new_min_stake: u128 },
    /// Change unbonding period (epochs).
    SetUnbondingPeriod { new_period: u64 },
    /// Emergency pause (circuit breaker).
    EmergencyPause,
    /// Resume from emergency pause.
    EmergencyResume,
    /// Set the authorized bridge relayer account that may release vault funds
    /// via `bridge_from_base`. Until set, bridge releases are disabled.
    SetBridgeRelayer { relayer: [u8; 32] },
    /// Establish or rotate the vesting-contract admin (the account allowed to
    /// add post-genesis vesting schedules). Until set, no admin exists and the
    /// admin-only vesting methods are disabled.
    SetVestingAdmin { admin: [u8; 32] },
    /// One-time treasury operation: move the entire balance of the team pool
    /// account into the vesting vault, so vesting claims are backed by real
    /// funds instead of minting. Idempotent (a second run moves 0). See the
    /// MigrateTeamPoolToVesting execution arm.
    MigrateTeamPoolToVesting,
    /// Set the governance voting period (in epochs). Lets governance correct
    /// the stored value once unfrozen; the create-time clamp
    /// (`effective_voting_period`) still guards against an absurd setting.
    SetGovernanceVotingPeriod { epochs: u64 },
}

/// Status of a governance proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus {
    Active,
    Passed,
    Rejected,
    Executed,
    Expired,
}

/// A vote cast on a proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vote {
    pub voter: AccountId,
    pub support: bool,
    pub stake_weight: u128,
}

/// A governance proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: u64,
    pub proposer: AccountId,
    pub action: ProposalAction,
    pub description: String,
    pub created_epoch: u64,
    pub voting_end_epoch: u64,
    pub execute_after_epoch: u64,
    pub status: ProposalStatus,
    pub votes: Vec<Vote>,
    pub total_for: u128,
    pub total_against: u128,
    /// Deposit paid by proposer (returned if passed, burned if rejected).
    #[serde(default)]
    pub deposit: u128,
}

/// The governance contract state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GovernanceContract {
    pub proposals: Vec<Proposal>,
    next_proposal_id: u64,
    pub is_paused: bool,
    /// Voting period in epochs (configurable via genesis).
    #[serde(default = "default_voting_period")]
    pub voting_period: u64,
}

fn default_voting_period() -> u64 {
    DEFAULT_VOTING_PERIOD
}

impl GovernanceContract {
    pub fn new() -> Self {
        Self {
            voting_period: DEFAULT_VOTING_PERIOD,
            ..Default::default()
        }
    }

    /// The voting period actually applied to new proposals, clamped so a
    /// misconfigured stored value can't freeze governance. Pure and
    /// deterministic — it reads, never mutates, stored state, so it carries no
    /// fork risk and needs no migration height. A stored value of 0 or above
    /// `MAX_SANE_VOTING_PERIOD` falls back to `SANE_VOTING_PERIOD`; any sane
    /// value (e.g. one later set via SetGovernanceVotingPeriod) is used as-is.
    pub fn effective_voting_period(&self) -> u64 {
        if self.voting_period == 0 || self.voting_period > MAX_SANE_VOTING_PERIOD {
            SANE_VOTING_PERIOD
        } else {
            self.voting_period
        }
    }

    /// Create a new proposal.
    pub fn create_proposal(
        &mut self,
        proposer: AccountId,
        action: ProposalAction,
        description: String,
        current_epoch: u64,
    ) -> u64 {
        let id = self.next_proposal_id;
        self.next_proposal_id += 1;

        let voting_end = current_epoch + self.effective_voting_period();
        let execute_after = voting_end + TIMELOCK_DELAY;

        self.proposals.push(Proposal {
            id,
            proposer,
            action,
            description,
            created_epoch: current_epoch,
            voting_end_epoch: voting_end,
            execute_after_epoch: execute_after,
            status: ProposalStatus::Active,
            votes: Vec::new(),
            total_for: 0,
            total_against: 0,
            deposit: PROPOSAL_DEPOSIT,
        });

        id
    }

    /// Cast a vote on a proposal.
    pub fn vote(
        &mut self,
        proposal_id: u64,
        voter: AccountId,
        support: bool,
        stake_weight: u128,
        current_epoch: u64,
    ) -> Result<(), GovernanceError> {
        let proposal = self
            .proposals
            .iter_mut()
            .find(|p| p.id == proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;

        if proposal.status != ProposalStatus::Active {
            return Err(GovernanceError::VotingEnded);
        }
        if current_epoch > proposal.voting_end_epoch {
            return Err(GovernanceError::VotingEnded);
        }
        if proposal.votes.iter().any(|v| v.voter == voter) {
            return Err(GovernanceError::AlreadyVoted);
        }

        if support {
            proposal.total_for += stake_weight;
        } else {
            proposal.total_against += stake_weight;
        }

        proposal.votes.push(Vote {
            voter,
            support,
            stake_weight,
        });

        Ok(())
    }

    /// Finalize a proposal after the voting period.
    pub fn finalize(
        &mut self,
        proposal_id: u64,
        total_stake: u128,
        current_epoch: u64,
        emergency_fasttrack: bool,
    ) -> Result<ProposalStatus, GovernanceError> {
        let proposal = self
            .proposals
            .iter_mut()
            .find(|p| p.id == proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;

        // Emergency circuit-breaker actions may finalize the instant they reach
        // quorum + supermajority — no voting-window wait. Every other proposal
        // must run its full voting period first.
        let emergency = emergency_fasttrack && is_emergency(&proposal.action);
        if !emergency && current_epoch <= proposal.voting_end_epoch {
            return Err(GovernanceError::VotingNotEnded);
        }

        if proposal.status != ProposalStatus::Active {
            return Ok(proposal.status.clone());
        }

        let total_voted = proposal.total_for + proposal.total_against;

        // Use saturating multiplication to prevent overflow with large stakes.
        // quorum: total_voted / total_stake >= QUORUM_BPS / 10_000
        let quorum_met = total_stake > 0
            && total_voted.saturating_mul(10_000) >= total_stake.saturating_mul(QUORUM_BPS as u128);

        // threshold: total_for / total_voted >= PASS_THRESHOLD_BPS / 10_000
        let threshold_met = total_voted > 0
            && proposal.total_for.saturating_mul(10_000) >= total_voted.saturating_mul(PASS_THRESHOLD_BPS as u128);

        if quorum_met && threshold_met {
            proposal.status = ProposalStatus::Passed;
        } else if emergency && current_epoch <= proposal.voting_end_epoch {
            // Emergency finalized early but not enough stake has voted yet — keep
            // it Active (retriable as votes arrive) rather than rejecting it.
            return Ok(ProposalStatus::Active);
        } else {
            proposal.status = ProposalStatus::Rejected;
        }

        Ok(proposal.status.clone())
    }

    /// Execute a passed proposal after the timelock.
    pub fn execute(
        &mut self,
        proposal_id: u64,
        current_epoch: u64,
        emergency_fasttrack: bool,
    ) -> Result<ProposalAction, GovernanceError> {
        let proposal = self
            .proposals
            .iter_mut()
            .find(|p| p.id == proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;

        if proposal.status != ProposalStatus::Passed {
            return Err(GovernanceError::ProposalNotPassed);
        }
        // Emergency actions execute immediately once Passed — no timelock. The
        // timelock exists to give the network a window to react to a controversial
        // change; a circuit breaker is the opposite of something you want delayed.
        let emergency = emergency_fasttrack && is_emergency(&proposal.action);
        if !emergency && current_epoch < proposal.execute_after_epoch {
            return Err(GovernanceError::TimelockNotExpired);
        }

        proposal.status = ProposalStatus::Executed;
        let action = proposal.action.clone();

        // Handle emergency actions immediately.
        match &action {
            ProposalAction::EmergencyPause => self.is_paused = true,
            ProposalAction::EmergencyResume => self.is_paused = false,
            _ => {}
        }

        Ok(action)
    }

    pub fn get_proposal(&self, id: u64) -> Option<&Proposal> {
        self.proposals.iter().find(|p| p.id == id)
    }

    const STORAGE_KEY: &'static [u8] = b"__governance_state__";

    pub fn load(store: &dyn solen_storage::StateStore) -> Self {
        match store.get(Self::STORAGE_KEY) {
            Ok(Some(data)) => serde_json::from_slice(&data).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    pub fn save(&self, store: &mut dyn solen_storage::StateStore) {
        if let Ok(data) = serde_json::to_vec(self) {
            let _ = store.put(Self::STORAGE_KEY, &data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aid(n: u8) -> AccountId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn full_governance_lifecycle() {
        let mut gov = GovernanceContract::new();

        // Create proposal at epoch 10.
        let pid = gov.create_proposal(
            aid(1),
            ProposalAction::SetBaseFee { new_fee: 5 },
            "Lower fees".into(),
            10,
        );

        // Vote with 70% for (above 66.67% threshold).
        gov.vote(pid, aid(10), true, 700, 12).unwrap();
        gov.vote(pid, aid(11), false, 300, 12).unwrap();

        // Can't finalize during voting period.
        assert!(gov.finalize(pid, 1000, 20, false).is_err());

        // Finalize after voting period (10 + 14 = 24).
        let status = gov.finalize(pid, 1000, 25, false).unwrap();
        assert_eq!(status, ProposalStatus::Passed);

        // Can't execute before timelock (24 + 3 = 27).
        assert!(gov.execute(pid, 26, false).is_err());

        // Execute after timelock.
        let action = gov.execute(pid, 28, false).unwrap();
        match action {
            ProposalAction::SetBaseFee { new_fee } => assert_eq!(new_fee, 5),
            _ => panic!("wrong action"),
        }

        assert_eq!(
            gov.get_proposal(pid).unwrap().status,
            ProposalStatus::Executed
        );
    }

    #[test]
    fn absurd_voting_period_is_clamped() {
        // Reproduce the mainnet misconfiguration: voting_period = 201,600 epochs.
        let mut gov = GovernanceContract::new();
        gov.voting_period = 201_600;
        assert_eq!(gov.effective_voting_period(), SANE_VOTING_PERIOD);

        // A proposal created at epoch 100 must end at 100 + SANE, not + 201,600,
        // so it can actually be finalized.
        let pid = gov.create_proposal(aid(1), ProposalAction::EmergencyPause, "x".into(), 100);
        assert_eq!(
            gov.get_proposal(pid).unwrap().voting_end_epoch,
            100 + SANE_VOTING_PERIOD
        );

        // A sane stored value is respected as-is (not overridden by the clamp).
        let mut gov2 = GovernanceContract::new();
        gov2.voting_period = 1_000;
        assert_eq!(gov2.effective_voting_period(), 1_000);
    }

    #[test]
    fn rejected_without_quorum() {
        let mut gov = GovernanceContract::new();
        let pid = gov.create_proposal(
            aid(1),
            ProposalAction::SetBlockTime { new_block_time_ms: 1000 },
            "Faster blocks".into(),
            0,
        );

        // Only 20% participation (below 30% quorum).
        gov.vote(pid, aid(10), true, 200, 5).unwrap();

        let status = gov.finalize(pid, 1000, 15, false).unwrap();
        assert_eq!(status, ProposalStatus::Rejected);
    }

    #[test]
    fn rejected_below_threshold() {
        let mut gov = GovernanceContract::new();
        let pid = gov.create_proposal(
            aid(1),
            ProposalAction::SetBaseFee { new_fee: 0 },
            "Free gas".into(),
            0,
        );

        // 50% for, 50% against — below 66.67% threshold.
        gov.vote(pid, aid(10), true, 500, 5).unwrap();
        gov.vote(pid, aid(11), false, 500, 5).unwrap();

        let status = gov.finalize(pid, 1000, 15, false).unwrap();
        assert_eq!(status, ProposalStatus::Rejected);
    }

    #[test]
    fn double_vote_rejected() {
        let mut gov = GovernanceContract::new();
        let pid = gov.create_proposal(
            aid(1),
            ProposalAction::EmergencyPause,
            "Pause".into(),
            0,
        );

        gov.vote(pid, aid(10), true, 100, 5).unwrap();
        let err = gov.vote(pid, aid(10), false, 100, 5).unwrap_err();
        assert!(matches!(err, GovernanceError::AlreadyVoted));
    }

    #[test]
    fn emergency_pause() {
        let mut gov = GovernanceContract::new();
        let pid = gov.create_proposal(
            aid(1),
            ProposalAction::EmergencyPause,
            "Emergency".into(),
            0,
        );

        gov.vote(pid, aid(10), true, 1000, 5).unwrap();
        gov.finalize(pid, 1000, 15, false).unwrap();
        gov.execute(pid, 20, false).unwrap();

        assert!(gov.is_paused);
    }

    // ── emergency fast-track (execute as soon as enough votes) ──────────────

    /// With the fast-track active, an emergency proposal finalizes + executes the
    /// instant it hits quorum + supermajority — no voting-window wait, no timelock.
    #[test]
    fn emergency_fasttrack_executes_immediately_on_quorum() {
        let mut gov = GovernanceContract::new();
        let pid = gov.create_proposal(aid(1), ProposalAction::EmergencyPause, "halt now".into(), 100);
        let voting_end = gov.get_proposal(pid).unwrap().voting_end_epoch; // 100 + 14
        assert!(voting_end > 101);

        // Enough stake votes yes (100% of 1000 = quorum + supermajority).
        gov.vote(pid, aid(10), true, 1000, 101).unwrap();

        // Finalize at epoch 101 — DEEP inside the voting window — succeeds only
        // because it's an emergency and the fast-track is active.
        let status = gov.finalize(pid, 1000, 101, true).unwrap();
        assert_eq!(status, ProposalStatus::Passed, "emergency passes early on quorum");

        // Execute immediately — no timelock wait.
        let action = gov.execute(pid, 101, true).unwrap();
        assert!(matches!(action, ProposalAction::EmergencyPause));
        assert!(gov.is_paused, "circuit breaker fired immediately");
    }

    /// An emergency finalized early with insufficient votes stays Active (does NOT
    /// reject) so it can pass as more stake votes.
    #[test]
    fn emergency_fasttrack_stays_active_until_enough_votes() {
        let mut gov = GovernanceContract::new();
        let pid = gov.create_proposal(aid(1), ProposalAction::EmergencyResume, "resume".into(), 100);

        // Only 20% votes — below the 30% quorum.
        gov.vote(pid, aid(10), true, 200, 101).unwrap();
        let status = gov.finalize(pid, 1000, 101, true).unwrap();
        assert_eq!(status, ProposalStatus::Active, "not enough yet — stays open, not rejected");

        // More stake votes → now over quorum + threshold → passes.
        gov.vote(pid, aid(11), true, 600, 102).unwrap();
        let status = gov.finalize(pid, 1000, 102, true).unwrap();
        assert_eq!(status, ProposalStatus::Passed);
        gov.execute(pid, 102, true).unwrap();
        assert!(!gov.is_paused);
    }

    /// The fast-track must NOT touch normal proposals — they still serve their
    /// full voting window even when the fast-track is active.
    #[test]
    fn fasttrack_does_not_affect_normal_proposals() {
        let mut gov = GovernanceContract::new();
        let pid = gov.create_proposal(aid(1), ProposalAction::SetBaseFee { new_fee: 5 }, "fee".into(), 100);
        gov.vote(pid, aid(10), true, 1000, 101).unwrap();
        // Even with emergency_fasttrack=true, a non-emergency can't finalize early.
        assert!(matches!(
            gov.finalize(pid, 1000, 101, true),
            Err(GovernanceError::VotingNotEnded)
        ));
    }

    /// While the gate is dormant (fast-track=false), an emergency proposal behaves
    /// like any other — full voting window + timelock.
    #[test]
    fn emergency_normal_when_fasttrack_dormant() {
        let mut gov = GovernanceContract::new();
        let pid = gov.create_proposal(aid(1), ProposalAction::EmergencyPause, "halt".into(), 100);
        gov.vote(pid, aid(10), true, 1000, 101).unwrap();
        assert!(matches!(
            gov.finalize(pid, 1000, 101, false),
            Err(GovernanceError::VotingNotEnded)
        ));
    }
}
