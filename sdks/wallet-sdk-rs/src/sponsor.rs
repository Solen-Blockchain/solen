//! Fee sponsorship / paymaster policy evaluation.

use solen_types::transaction::UserOperation;
use solen_types::AccountId;

/// A sponsorship policy that determines if an operation qualifies
/// for fee sponsorship by a paymaster.
#[derive(Debug, Clone)]
pub struct SponsorshipPolicy {
    /// The paymaster account that pays fees.
    pub sponsor: AccountId,
    /// Maximum gas per sponsored operation.
    pub max_gas_per_op: u64,
    /// Maximum total daily spend.
    pub max_daily_spend: u128,
    /// Allowed contract targets (empty = allow all).
    pub allowed_targets: Vec<AccountId>,
    /// Current daily spend (tracked externally).
    pub current_daily_spend: u128,
}

/// Result of a sponsorship check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SponsorshipDecision {
    Approved,
    DeniedOverBudget,
    DeniedTargetNotAllowed,
    DeniedGasTooHigh,
}

impl SponsorshipPolicy {
    pub fn new(sponsor: AccountId) -> Self {
        Self {
            sponsor,
            max_gas_per_op: 100_000,
            max_daily_spend: 1_000_000,
            allowed_targets: Vec::new(),
            current_daily_spend: 0,
        }
    }

    /// Check if an operation qualifies for sponsorship.
    pub fn check(&self, op: &UserOperation, estimated_gas: u64) -> SponsorshipDecision {
        if estimated_gas > self.max_gas_per_op {
            return SponsorshipDecision::DeniedGasTooHigh;
        }

        let estimated_fee = estimated_gas as u128;
        if self.current_daily_spend + estimated_fee > self.max_daily_spend {
            return SponsorshipDecision::DeniedOverBudget;
        }

        if !self.allowed_targets.is_empty() {
            use solen_types::transaction::Action;
            for action in &op.actions {
                match action {
                    Action::Call { target, .. } => {
                        if !self.allowed_targets.contains(target) {
                            return SponsorshipDecision::DeniedTargetNotAllowed;
                        }
                    }
                    _ => {}
                }
            }
        }

        SponsorshipDecision::Approved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solen_types::transaction::Action;

    fn aid(n: u8) -> AccountId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    fn op_with_call(target: AccountId) -> UserOperation {
        UserOperation {
            sender: aid(1),
            nonce: 0,
            actions: vec![Action::Call {
                target,
                method: "test".into(),
                args: vec![],
            }],
            max_fee: 1000,
            signature: vec![],
        }
    }

    #[test]
    fn approved_within_limits() {
        let policy = SponsorshipPolicy::new(aid(99));
        let op = op_with_call(aid(10));
        assert_eq!(policy.check(&op, 1000), SponsorshipDecision::Approved);
    }

    #[test]
    fn denied_gas_too_high() {
        let mut policy = SponsorshipPolicy::new(aid(99));
        policy.max_gas_per_op = 500;
        let op = op_with_call(aid(10));
        assert_eq!(policy.check(&op, 1000), SponsorshipDecision::DeniedGasTooHigh);
    }

    #[test]
    fn denied_target_not_allowed() {
        let mut policy = SponsorshipPolicy::new(aid(99));
        policy.allowed_targets = vec![aid(10)];
        let op = op_with_call(aid(20)); // not in allowed list
        assert_eq!(
            policy.check(&op, 100),
            SponsorshipDecision::DeniedTargetNotAllowed
        );
    }
}
