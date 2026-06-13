//! Intent pool: collects pending intents and matches them with solver solutions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use solen_types::AccountId;
use thiserror::Error;
use tracing::info;

use crate::types::{Intent, IntentStatus, Solution};

#[derive(Debug, Error)]
pub enum PoolError {
    #[error("intent pool is full")]
    Full,
    #[error("intent not found: {0}")]
    NotFound(u64),
    #[error("intent already fulfilled")]
    AlreadyFulfilled,
    #[error("invalid solver signature")]
    InvalidSignature,
    #[error("intent expired")]
    Expired,
    #[error("no solution submitted for intent {0}")]
    NoSolution(u64),
}

/// The intent pool collects intents and their solutions.
pub struct IntentPool {
    intents: Arc<Mutex<HashMap<u64, (Intent, IntentStatus)>>>,
    solutions: Arc<Mutex<HashMap<u64, Vec<Solution>>>>,
    next_id: Arc<Mutex<u64>>,
    max_size: usize,
}

impl IntentPool {
    pub fn new(max_size: usize) -> Self {
        Self {
            intents: Arc::new(Mutex::new(HashMap::new())),
            solutions: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(0)),
            max_size,
        }
    }

    /// Submit an intent to the pool. Returns the assigned intent ID.
    pub fn submit(&self, mut intent: Intent) -> Result<u64, PoolError> {
        let mut intents = self.intents.lock().unwrap();
        if intents.len() >= self.max_size {
            return Err(PoolError::Full);
        }

        let mut id = self.next_id.lock().unwrap();
        intent.id = *id;
        *id += 1;
        let intent_id = intent.id;

        intents.insert(intent_id, (intent, IntentStatus::Pending));
        Ok(intent_id)
    }

    /// Submit a solver's solution for an intent.
    /// The solution must include a valid signature proving the submitter
    /// controls the claimed solver account, preventing MEV manipulation
    /// by fake solvers.
    pub fn submit_solution(&self, solution: Solution) -> Result<(), PoolError> {
        // Require a valid solver signature over intent_id[8] + solver[32] +
        // claimed_tip[16]. This authenticates the `solver` field so a submitter
        // cannot attribute solutions to a competitor or inflate `score` under
        // another identity to win select_best_solution. The in-process built-in
        // solver does NOT go through this method (produce_block calls it
        // directly), so requiring a signature here only constrains external
        // RPC submissions — which must always be signed.
        if solution.signature.len() != 64 {
            return Err(PoolError::InvalidSignature);
        }
        {
            let mut msg = Vec::with_capacity(56);
            msg.extend_from_slice(&solution.intent_id.to_le_bytes());
            msg.extend_from_slice(&solution.solver);
            msg.extend_from_slice(&solution.claimed_tip.to_le_bytes());
            let mut sig = [0u8; 64];
            sig.copy_from_slice(&solution.signature);
            if solen_crypto::verify(&solution.solver, &msg, &sig).is_err() {
                return Err(PoolError::InvalidSignature);
            }
        }

        let intents = self.intents.lock().unwrap();
        let (_, status) = intents
            .get(&solution.intent_id)
            .ok_or(PoolError::NotFound(solution.intent_id))?;

        if *status == IntentStatus::Fulfilled {
            return Err(PoolError::AlreadyFulfilled);
        }

        drop(intents);

        let mut solutions = self.solutions.lock().unwrap();
        let entry = solutions.entry(solution.intent_id).or_default();

        // Cap solutions per intent to prevent memory exhaustion.
        const MAX_SOLUTIONS_PER_INTENT: usize = 50;
        if entry.len() >= MAX_SOLUTIONS_PER_INTENT {
            return Err(PoolError::Full);
        }

        entry.push(solution);
        Ok(())
    }

    /// Select the best solution for an intent (highest score, lowest tip claim).
    pub fn select_best_solution(&self, intent_id: u64) -> Result<Solution, PoolError> {
        let solutions = self.solutions.lock().unwrap();
        let candidates = solutions
            .get(&intent_id)
            .ok_or(PoolError::NoSolution(intent_id))?;

        if candidates.is_empty() {
            return Err(PoolError::NoSolution(intent_id));
        }

        // Select by highest score, then lowest tip claim.
        let best = candidates
            .iter()
            .max_by(|a, b| {
                a.score
                    .cmp(&b.score)
                    .then_with(|| b.claimed_tip.cmp(&a.claimed_tip))
            })
            .unwrap()
            .clone();

        Ok(best)
    }

    /// Mark an intent as fulfilled.
    pub fn fulfill(&self, intent_id: u64) -> Result<(), PoolError> {
        let mut intents = self.intents.lock().unwrap();
        let (_, status) = intents
            .get_mut(&intent_id)
            .ok_or(PoolError::NotFound(intent_id))?;

        *status = IntentStatus::Fulfilled;
        info!(intent_id, "intent fulfilled");
        Ok(())
    }

    /// Cancel an intent (only by the sender).
    pub fn cancel(&self, intent_id: u64, sender: &AccountId) -> Result<(), PoolError> {
        let mut intents = self.intents.lock().unwrap();
        let (intent, status) = intents
            .get_mut(&intent_id)
            .ok_or(PoolError::NotFound(intent_id))?;

        if intent.sender != *sender {
            return Err(PoolError::NotFound(intent_id));
        }

        *status = IntentStatus::Cancelled;
        Ok(())
    }

    /// Expire all intents past their expiry height.
    pub fn expire(&self, current_height: u64) -> usize {
        let mut intents = self.intents.lock().unwrap();
        let mut count = 0;
        for (_, (intent, status)) in intents.iter_mut() {
            if *status == IntentStatus::Pending && current_height > intent.expiry_height {
                *status = IntentStatus::Expired;
                count += 1;
            }
        }
        count
    }

    /// Get all pending intents.
    pub fn pending_intents(&self) -> Vec<Intent> {
        let intents = self.intents.lock().unwrap();
        intents
            .values()
            .filter(|(_, s)| *s == IntentStatus::Pending)
            .map(|(i, _)| i.clone())
            .collect()
    }

    /// Number of pending intents.
    pub fn pending_count(&self) -> usize {
        let intents = self.intents.lock().unwrap();
        intents
            .values()
            .filter(|(_, s)| *s == IntentStatus::Pending)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Constraint;

    fn aid(n: u8) -> AccountId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    fn test_intent() -> Intent {
        Intent {
            id: 0,
            sender: aid(1),
            constraints: vec![Constraint::MinBalance {
                account: aid(1),
                min_amount: 500,
            }],
            max_fee: 1000,
            expiry_height: 100,
            signature: vec![],
            tip: 50,
        }
    }

    fn test_solution(intent_id: u64, score: u64) -> Solution {
        // Solver solutions must now carry a valid signature over
        // intent_id[8] + solver[32] + claimed_tip[16], so derive the solver
        // from a real keypair and sign.
        let kp = solen_crypto::Keypair::from_seed(&[10u8; 32]);
        let solver = kp.public_key();
        let claimed_tip: u128 = 25;
        let mut msg = Vec::with_capacity(56);
        msg.extend_from_slice(&intent_id.to_le_bytes());
        msg.extend_from_slice(&solver);
        msg.extend_from_slice(&claimed_tip.to_le_bytes());
        let signature = kp.sign(&msg).to_vec();
        Solution {
            intent_id,
            solver,
            operations: vec![],
            claimed_tip,
            score,
            signature,
        }
    }

    #[test]
    fn submit_and_fulfill() {
        let pool = IntentPool::new(100);
        let id = pool.submit(test_intent()).unwrap();

        assert_eq!(pool.pending_count(), 1);

        pool.submit_solution(test_solution(id, 100)).unwrap();
        let best = pool.select_best_solution(id).unwrap();
        assert_eq!(best.score, 100);

        pool.fulfill(id).unwrap();
        assert_eq!(pool.pending_count(), 0);
    }

    #[test]
    fn best_solution_selected() {
        let pool = IntentPool::new(100);
        let id = pool.submit(test_intent()).unwrap();

        pool.submit_solution(test_solution(id, 50)).unwrap();
        pool.submit_solution(test_solution(id, 100)).unwrap();
        pool.submit_solution(test_solution(id, 75)).unwrap();

        let best = pool.select_best_solution(id).unwrap();
        assert_eq!(best.score, 100);
    }

    #[test]
    fn expire_intents() {
        let pool = IntentPool::new(100);
        pool.submit(test_intent()).unwrap(); // expires at height 100

        assert_eq!(pool.expire(50), 0);
        assert_eq!(pool.expire(200), 1);
        assert_eq!(pool.pending_count(), 0);
    }

    #[test]
    fn cancel_by_sender() {
        let pool = IntentPool::new(100);
        let id = pool.submit(test_intent()).unwrap();

        pool.cancel(id, &aid(1)).unwrap();
        assert_eq!(pool.pending_count(), 0);

        // Wrong sender can't cancel
        let id2 = pool.submit(test_intent()).unwrap();
        assert!(pool.cancel(id2, &aid(99)).is_err());
    }
}
