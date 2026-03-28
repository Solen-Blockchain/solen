//! Governance contract: proposals, stake-weighted voting, timelocked execution.
//!
//! Solen favors minimal governance. Only narrow parameter changes are
//! allowed through on-chain governance. Core protocol changes require
//! multi-phase qualification.

use serde::{Deserialize, Serialize};
use solen_types::AccountId;
use thiserror::Error;

/// Voting period in epochs.
pub const VOTING_PERIOD: u64 = 14;

/// Timelock delay after passing (in epochs).
pub const TIMELOCK_DELAY: u64 = 3;

/// Quorum: minimum participation as basis points of total stake.
pub const QUORUM_BPS: u64 = 3000; // 30%

/// Supermajority threshold for passing (basis points).
pub const PASS_THRESHOLD_BPS: u64 = 6667; // 66.67%

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
    /// Emergency pause (circuit breaker).
    EmergencyPause,
    /// Resume from emergency pause.
    EmergencyResume,
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
}

/// The governance contract state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GovernanceContract {
    pub proposals: Vec<Proposal>,
    next_proposal_id: u64,
    pub is_paused: bool,
}

impl GovernanceContract {
    pub fn new() -> Self {
        Self::default()
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

        let voting_end = current_epoch + VOTING_PERIOD;
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
    ) -> Result<ProposalStatus, GovernanceError> {
        let proposal = self
            .proposals
            .iter_mut()
            .find(|p| p.id == proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;

        if current_epoch <= proposal.voting_end_epoch {
            return Err(GovernanceError::VotingNotEnded);
        }

        if proposal.status != ProposalStatus::Active {
            return Ok(proposal.status.clone());
        }

        let total_voted = proposal.total_for + proposal.total_against;
        let quorum_met = total_stake > 0
            && total_voted * 10_000 / total_stake >= QUORUM_BPS as u128;

        let threshold_met = total_voted > 0
            && proposal.total_for * 10_000 / total_voted >= PASS_THRESHOLD_BPS as u128;

        proposal.status = if quorum_met && threshold_met {
            ProposalStatus::Passed
        } else {
            ProposalStatus::Rejected
        };

        Ok(proposal.status.clone())
    }

    /// Execute a passed proposal after the timelock.
    pub fn execute(
        &mut self,
        proposal_id: u64,
        current_epoch: u64,
    ) -> Result<ProposalAction, GovernanceError> {
        let proposal = self
            .proposals
            .iter_mut()
            .find(|p| p.id == proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;

        if proposal.status != ProposalStatus::Passed {
            return Err(GovernanceError::ProposalNotPassed);
        }
        if current_epoch < proposal.execute_after_epoch {
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
        assert!(gov.finalize(pid, 1000, 20).is_err());

        // Finalize after voting period (10 + 14 = 24).
        let status = gov.finalize(pid, 1000, 25).unwrap();
        assert_eq!(status, ProposalStatus::Passed);

        // Can't execute before timelock (24 + 3 = 27).
        assert!(gov.execute(pid, 26).is_err());

        // Execute after timelock.
        let action = gov.execute(pid, 28).unwrap();
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

        let status = gov.finalize(pid, 1000, 15).unwrap();
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

        let status = gov.finalize(pid, 1000, 15).unwrap();
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
        gov.finalize(pid, 1000, 15).unwrap();
        gov.execute(pid, 20).unwrap();

        assert!(gov.is_paused);
    }
}
