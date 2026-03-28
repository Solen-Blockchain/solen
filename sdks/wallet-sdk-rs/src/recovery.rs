//! Account recovery: guardian-based threshold recovery with timelock.

use solen_types::AccountId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("not enough approvals: have {have}, need {need}")]
    InsufficientApprovals { have: usize, need: usize },
    #[error("timelock not expired")]
    TimelockActive,
    #[error("already approved by this guardian")]
    DuplicateApproval,
    #[error("not a guardian")]
    NotAGuardian,
}

/// A recovery request to change account ownership.
#[derive(Debug, Clone)]
pub struct RecoveryRequest {
    pub account: AccountId,
    pub new_owner: [u8; 32],
    pub approvals: Vec<AccountId>,
    pub initiated_at_epoch: u64,
    pub timelock_epochs: u64,
}

/// Manages guardian-based account recovery.
pub struct RecoveryManager {
    pub account: AccountId,
    pub guardians: Vec<AccountId>,
    pub threshold: usize,
    pub timelock_epochs: u64,
    pub pending_request: Option<RecoveryRequest>,
}

impl RecoveryManager {
    pub fn new(
        account: AccountId,
        guardians: Vec<AccountId>,
        threshold: usize,
        timelock_epochs: u64,
    ) -> Self {
        Self {
            account,
            guardians,
            threshold,
            timelock_epochs,
            pending_request: None,
        }
    }

    /// Initiate a recovery request.
    pub fn initiate(&mut self, new_owner: [u8; 32], current_epoch: u64) {
        self.pending_request = Some(RecoveryRequest {
            account: self.account,
            new_owner,
            approvals: Vec::new(),
            initiated_at_epoch: current_epoch,
            timelock_epochs: self.timelock_epochs,
        });
    }

    /// A guardian approves the pending recovery request.
    pub fn approve(&mut self, guardian: &AccountId) -> Result<(), RecoveryError> {
        if !self.guardians.contains(guardian) {
            return Err(RecoveryError::NotAGuardian);
        }

        let request = self
            .pending_request
            .as_mut()
            .ok_or(RecoveryError::InsufficientApprovals {
                have: 0,
                need: self.threshold,
            })?;

        if request.approvals.contains(guardian) {
            return Err(RecoveryError::DuplicateApproval);
        }

        request.approvals.push(*guardian);
        Ok(())
    }

    /// Check if the recovery can be executed.
    pub fn can_execute(&self, current_epoch: u64) -> Result<(), RecoveryError> {
        let request = self
            .pending_request
            .as_ref()
            .ok_or(RecoveryError::InsufficientApprovals {
                have: 0,
                need: self.threshold,
            })?;

        if request.approvals.len() < self.threshold {
            return Err(RecoveryError::InsufficientApprovals {
                have: request.approvals.len(),
                need: self.threshold,
            });
        }

        if current_epoch < request.initiated_at_epoch + request.timelock_epochs {
            return Err(RecoveryError::TimelockActive);
        }

        Ok(())
    }

    /// Execute the recovery (returns the new owner key).
    pub fn execute(&mut self, current_epoch: u64) -> Result<[u8; 32], RecoveryError> {
        self.can_execute(current_epoch)?;
        let request = self.pending_request.take().unwrap();
        Ok(request.new_owner)
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
    fn full_recovery_flow() {
        let mut rm = RecoveryManager::new(
            aid(1),
            vec![aid(10), aid(11), aid(12)],
            2,  // 2-of-3
            3,  // 3 epoch timelock
        );

        rm.initiate([99u8; 32], 10);

        rm.approve(&aid(10)).unwrap();
        rm.approve(&aid(11)).unwrap();

        // Too early.
        assert!(rm.can_execute(12).is_err());

        // After timelock (10 + 3 = 13).
        let new_owner = rm.execute(13).unwrap();
        assert_eq!(new_owner, [99u8; 32]);
    }

    #[test]
    fn insufficient_approvals() {
        let mut rm = RecoveryManager::new(aid(1), vec![aid(10), aid(11)], 2, 0);
        rm.initiate([99u8; 32], 0);
        rm.approve(&aid(10)).unwrap();

        assert!(matches!(
            rm.can_execute(100),
            Err(RecoveryError::InsufficientApprovals { have: 1, need: 2 })
        ));
    }

    #[test]
    fn non_guardian_rejected() {
        let mut rm = RecoveryManager::new(aid(1), vec![aid(10)], 1, 0);
        rm.initiate([99u8; 32], 0);
        assert!(matches!(rm.approve(&aid(50)), Err(RecoveryError::NotAGuardian)));
    }
}
