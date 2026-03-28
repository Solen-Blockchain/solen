//! Spending policies, session credentials, and approval rules.

use solen_types::transaction::UserOperation;
use solen_types::AccountId;

/// A spending policy rule.
#[derive(Debug, Clone)]
pub enum PolicyRule {
    /// Maximum spend per operation.
    MaxPerTransaction(u128),
    /// Only allow transfers to specific recipients.
    AllowedRecipients(Vec<AccountId>),
    /// Block all operations (account frozen).
    Frozen,
}

/// Result of policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny(String),
}

/// Evaluates operations against a set of policy rules.
pub struct PolicyEngine {
    rules: Vec<PolicyRule>,
}

impl PolicyEngine {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn add_rule(mut self, rule: PolicyRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// Evaluate an operation against all policy rules.
    pub fn evaluate(&self, op: &UserOperation) -> PolicyDecision {
        use solen_types::transaction::Action;

        for rule in &self.rules {
            match rule {
                PolicyRule::Frozen => {
                    return PolicyDecision::Deny("account is frozen".into());
                }
                PolicyRule::MaxPerTransaction(max) => {
                    let total_spend: u128 = op
                        .actions
                        .iter()
                        .filter_map(|a| match a {
                            Action::Transfer { amount, .. } => Some(*amount),
                            _ => None,
                        })
                        .sum();
                    if total_spend > *max {
                        return PolicyDecision::Deny(format!(
                            "exceeds per-tx limit: {total_spend} > {max}"
                        ));
                    }
                }
                PolicyRule::AllowedRecipients(allowed) => {
                    for action in &op.actions {
                        if let Action::Transfer { to, .. } = action {
                            if !allowed.contains(to) {
                                return PolicyDecision::Deny(
                                    "recipient not in allowed list".into(),
                                );
                            }
                        }
                    }
                }
            }
        }

        PolicyDecision::Allow
    }
}

impl Default for PolicyEngine {
    fn default() -> Self {
        Self::new()
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

    fn transfer_op(to: AccountId, amount: u128) -> UserOperation {
        UserOperation {
            sender: aid(1),
            nonce: 0,
            actions: vec![Action::Transfer { to, amount }],
            max_fee: 1000,
            signature: vec![],
        }
    }

    #[test]
    fn allow_within_limits() {
        let engine = PolicyEngine::new()
            .add_rule(PolicyRule::MaxPerTransaction(1000));

        assert_eq!(
            engine.evaluate(&transfer_op(aid(2), 500)),
            PolicyDecision::Allow
        );
    }

    #[test]
    fn deny_over_limit() {
        let engine = PolicyEngine::new()
            .add_rule(PolicyRule::MaxPerTransaction(100));

        assert!(matches!(
            engine.evaluate(&transfer_op(aid(2), 500)),
            PolicyDecision::Deny(_)
        ));
    }

    #[test]
    fn deny_frozen() {
        let engine = PolicyEngine::new().add_rule(PolicyRule::Frozen);
        assert!(matches!(
            engine.evaluate(&transfer_op(aid(2), 1)),
            PolicyDecision::Deny(_)
        ));
    }

    #[test]
    fn allowed_recipients() {
        let engine = PolicyEngine::new()
            .add_rule(PolicyRule::AllowedRecipients(vec![aid(2)]));

        assert_eq!(
            engine.evaluate(&transfer_op(aid(2), 100)),
            PolicyDecision::Allow
        );
        assert!(matches!(
            engine.evaluate(&transfer_op(aid(3), 100)),
            PolicyDecision::Deny(_)
        ));
    }
}
