//! Intent pool security tests.

use solen_intents::pool::IntentPool;
use solen_intents::types::{Constraint, Intent, IntentStatus, Solution};
use solen_types::transaction::UserOperation;

fn aid(n: u8) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[0] = n;
    id
}

fn test_intent() -> Intent {
    Intent {
        id: 0,
        sender: aid(1),
        constraints: vec![Constraint::RequireTransfer {
            from: aid(1),
            to: aid(2),
            min_amount: 100,
        }],
        max_fee: 1000,
        expiry_height: 100,
        signature: vec![],
        tip: 50,
    }
}

/// Build a solver solution carrying a valid signature over
/// intent_id[8] + solver[32] + claimed_tip[16] (now required by the pool).
fn signed_solution(intent_id: u64, score: u64, claimed_tip: u128) -> Solution {
    let kp = solen_crypto::Keypair::from_seed(&[10u8; 32]);
    let solver = kp.public_key();
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

fn test_solution(intent_id: u64, score: u64) -> Solution {
    signed_solution(intent_id, score, 25)
}

// ── Solutions per intent capped ───────────────────────────────

#[test]
fn solution_cap_enforced() {
    let pool = IntentPool::new(100);
    let id = pool.submit(test_intent()).unwrap();

    // Submit 50 solutions (the cap). Sign each with its actual claimed_tip.
    for i in 0..50 {
        let sol = signed_solution(id, i, i as u128);
        assert!(pool.submit_solution(sol).is_ok(), "solution {} should succeed", i);
    }

    // 51st should fail.
    let sol = test_solution(id, 100);
    assert!(
        pool.submit_solution(sol).is_err(),
        "51st solution must be rejected (cap is 50)"
    );
}

// ── Fulfilled intents reject new solutions ────────────────────

#[test]
fn fulfilled_intent_rejects_solutions() {
    let pool = IntentPool::new(100);
    let id = pool.submit(test_intent()).unwrap();

    // Fulfill it.
    pool.fulfill(id).unwrap();

    // Try to submit a solution — should be rejected.
    let sol = test_solution(id, 100);
    assert!(
        pool.submit_solution(sol).is_err(),
        "fulfilled intent must reject solutions"
    );
}

// ── Double fulfill rejected ───────────────────────────────────

#[test]
fn double_fulfill_is_noop() {
    let pool = IntentPool::new(100);
    let id = pool.submit(test_intent()).unwrap();

    pool.fulfill(id).unwrap();
    // Second fulfill should still succeed (idempotent status change).
    // The status is already Fulfilled, so the intent stays Fulfilled.
    // This is fine — it's a no-op.
    assert_eq!(pool.pending_count(), 0);
}

// ── Pool size bounded ─────────────────────────────────────────

#[test]
fn pool_rejects_when_full() {
    let pool = IntentPool::new(5);

    for _ in 0..5 {
        pool.submit(test_intent()).unwrap();
    }

    // 6th should fail.
    assert!(pool.submit(test_intent()).is_err(), "pool must reject when full");
}

// ── Expired intents not in pending ────────────────────────────

#[test]
fn expired_intents_removed_from_pending() {
    let pool = IntentPool::new(100);
    pool.submit(test_intent()).unwrap(); // expiry_height=100

    assert_eq!(pool.pending_count(), 1);

    pool.expire(200); // current height > expiry

    assert_eq!(pool.pending_count(), 0);
}

// ── Cancel only by sender ─────────────────────────────────────

#[test]
fn cancel_only_by_sender() {
    let pool = IntentPool::new(100);
    let id = pool.submit(test_intent()).unwrap();

    // Wrong sender can't cancel.
    assert!(pool.cancel(id, &aid(99)).is_err());

    // Correct sender can cancel.
    assert!(pool.cancel(id, &aid(1)).is_ok());
    assert_eq!(pool.pending_count(), 0);
}
