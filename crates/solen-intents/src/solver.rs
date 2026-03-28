//! Solver interface: solvers compete to fulfill user intents.

use solen_types::AccountId;

use crate::types::{Intent, Solution};

/// Trait for intent solvers. Implementations compete to produce
/// the best solution for user intents.
pub trait IntentSolver: Send + Sync {
    /// Attempt to solve an intent. Returns None if this solver
    /// cannot handle the intent.
    fn solve(&self, intent: &Intent) -> Option<Solution>;

    /// The solver's identifier.
    fn solver_id(&self) -> AccountId;
}

/// A simple solver that converts transfer intents directly to operations.
pub struct DirectTransferSolver {
    pub id: AccountId,
}

impl IntentSolver for DirectTransferSolver {
    fn solve(&self, intent: &Intent) -> Option<Solution> {
        use crate::types::Constraint;
        use solen_types::transaction::{Action, UserOperation};

        // Look for RequireTransfer constraints and convert to operations.
        let mut actions = Vec::new();
        for constraint in &intent.constraints {
            if let Constraint::RequireTransfer {
                from: _,
                to,
                min_amount,
            } = constraint
            {
                actions.push(Action::Transfer {
                    to: *to,
                    amount: *min_amount,
                });
            }
        }

        if actions.is_empty() {
            return None;
        }

        let op = UserOperation {
            sender: intent.sender,
            nonce: 0, // solver must look up the actual nonce
            actions,
            max_fee: intent.max_fee,
            signature: vec![], // solver signs after constructing
        };

        Some(Solution {
            intent_id: intent.id,
            solver: self.id,
            operations: vec![op],
            claimed_tip: intent.tip / 2, // claim half the tip
            score: 100,
        })
    }

    fn solver_id(&self) -> AccountId {
        self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Constraint, Intent};

    fn aid(n: u8) -> AccountId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn direct_solver_handles_transfer() {
        let solver = DirectTransferSolver { id: aid(10) };

        let intent = Intent {
            id: 1,
            sender: aid(1),
            constraints: vec![Constraint::RequireTransfer {
                from: aid(1),
                to: aid(2),
                min_amount: 500,
            }],
            max_fee: 1000,
            expiry_height: 100,
            signature: vec![],
            tip: 100,
        };

        let solution = solver.solve(&intent).unwrap();
        assert_eq!(solution.operations.len(), 1);
        assert_eq!(solution.claimed_tip, 50);
    }

    #[test]
    fn direct_solver_skips_non_transfer() {
        let solver = DirectTransferSolver { id: aid(10) };

        let intent = Intent {
            id: 1,
            sender: aid(1),
            constraints: vec![Constraint::MinBalance {
                account: aid(1),
                min_amount: 500,
            }],
            max_fee: 1000,
            expiry_height: 100,
            signature: vec![],
            tip: 100,
        };

        assert!(solver.solve(&intent).is_none());
    }
}
