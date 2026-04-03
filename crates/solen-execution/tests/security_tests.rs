//! Security regression tests for the execution engine.
//!
//! These tests cover proven past bugs and critical invariants.

use solen_crypto::Keypair;
use solen_execution::executor::BlockExecutor;
use solen_execution::fees::FeeConfig;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_execution::state::StateManager;
use solen_storage::MemoryStore;
use solen_types::account::AuthMethod;
use solen_types::transaction::{Action, UserOperation};
use solen_types::AccountId;

fn make_id(n: u32) -> AccountId {
    let mut id = [0u8; 32];
    id[..4].copy_from_slice(&n.to_le_bytes());
    id
}

fn setup() -> (MemoryStore, Keypair, AccountId, AccountId) {
    let mut store = MemoryStore::new();
    let alice_kp = Keypair::from_seed(&[0x0A; 32]);
    let alice = alice_kp.public_key();
    let bob_kp = Keypair::from_seed(&[0x0B; 32]);
    let bob = bob_kp.public_key();

    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: alice,
                balance: 1_000_000_000,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: alice }],
            },
            GenesisAccount {
                id: bob,
                balance: 500_000_000,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: bob }],
            },
        ],
    )
    .unwrap();

    (store, alice_kp, alice, bob)
}

fn zero_fee_executor() -> BlockExecutor {
    BlockExecutor::with_fee_config(FeeConfig {
        base_fee_per_gas: 0,
        ..Default::default()
    })
}

fn sign_op(kp: &Keypair, executor: &BlockExecutor, op: &mut UserOperation) {
    let msg = executor.operation_signing_message(op);
    op.signature = kp.sign(&msg).to_vec();
}

// ── Test #11: Withdraw is idempotent ──────────────────────────

#[test]
fn withdraw_is_idempotent() {
    let mut store = MemoryStore::new();
    let kp = Keypair::from_seed(&[0x01; 32]);
    let validator_id = kp.public_key();

    apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: validator_id,
            balance: 10_000_000_000_000, // 100K SOLEN
            auth_methods: vec![AuthMethod::Ed25519 {
                public_key: validator_id,
            }],
        }],
    )
    .unwrap();

    // Register validator with self-stake.
    let mut staking =
        solen_system_contracts::staking::StakingContract::load(&store);
    staking
        .register_validator(validator_id, 50_000_000_000_000)
        .unwrap();
    staking.save(&mut store);

    // Delegate to self so we can undelegate.
    let mut staking =
        solen_system_contracts::staking::StakingContract::load(&store);
    staking
        .delegate(validator_id, validator_id, 10_000_000_000_000)
        .unwrap();
    staking.save(&mut store);

    // Undelegate the delegation.
    let mut staking =
        solen_system_contracts::staking::StakingContract::load(&store);
    staking
        .undelegate(validator_id, validator_id, 10_000_000_000_000, 0)
        .unwrap();
    staking.save(&mut store);

    // Advance past unbonding (7 epochs).
    let epoch = 10;

    // First withdraw — should return the undelegated amount.
    let mut staking =
        solen_system_contracts::staking::StakingContract::load(&store);
    let withdrawn1 = staking.withdraw_undelegated(validator_id, epoch);
    assert_eq!(withdrawn1, 10_000_000_000_000);
    staking.save(&mut store);

    // Second withdraw — should return 0 (already withdrawn).
    let mut staking =
        solen_system_contracts::staking::StakingContract::load(&store);
    let withdrawn2 = staking.withdraw_undelegated(validator_id, epoch);
    assert_eq!(withdrawn2, 0, "second withdraw should return 0");
    staking.save(&mut store);

    // Ten more withdraws — all 0.
    for _ in 0..10 {
        let mut staking =
            solen_system_contracts::staking::StakingContract::load(&store);
        let w = staking.withdraw_undelegated(validator_id, epoch);
        assert_eq!(w, 0, "repeated withdraw should always return 0");
        staking.save(&mut store);
    }
}

// ── Test #12: Multi-action rollback restores full state ───────

#[test]
fn multi_action_rollback_restores_full_state() {
    let (mut store, kp, alice, bob) = setup();
    let executor = zero_fee_executor();

    let initial_balance = {
        let state = StateManager::new(&mut store);
        state.require_account(&alice).unwrap().balance
    };

    // Two-action op: Transfer(1) then Transfer(999999999999) — second will fail.
    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![
            Action::Transfer {
                to: bob,
                amount: 1,
            },
            Action::Transfer {
                to: bob,
                amount: 999_999_999_999, // way more than balance
            },
        ],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success, "multi-action should fail");

    // Verify: alice's balance fully restored (minus gas fee if any).
    let state = StateManager::new(&mut store);
    let alice_after = state.require_account(&alice).unwrap();
    assert_eq!(
        alice_after.balance, initial_balance,
        "balance must be fully restored after multi-action failure"
    );

    // Verify: nonce was consumed (failure still burns nonce).
    assert_eq!(alice_after.nonce, 1, "nonce must be consumed even on failure");

    // Verify: bob's balance unchanged.
    let bob_after = state.require_account(&bob).unwrap();
    assert_eq!(bob_after.balance, 500_000_000, "bob's balance must be unchanged");
}

// Test #14 (intent [0xFF] mempool rejection) is in solen-consensus/tests/adversarial_tests.rs

// ── Test #25: No balance wrapping after block execution ───────

#[test]
fn no_balance_wrapping_after_execution() {
    let (mut store, kp, alice, bob) = setup();
    let executor = zero_fee_executor();

    // Transfer more than alice has — should fail, not wrap.
    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer {
            to: bob,
            amount: u128::MAX,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "transfer exceeding balance must fail"
    );

    // Verify balances are unchanged.
    let state = StateManager::new(&mut store);
    let alice_bal = state.require_account(&alice).unwrap().balance;
    let bob_bal = state.require_account(&bob).unwrap().balance;
    assert_eq!(alice_bal, 1_000_000_000);
    assert_eq!(bob_bal, 500_000_000);
}

// ── Test #26: Nonce never decreases across blocks ─────────────

#[test]
fn nonce_never_decreases() {
    let (mut store, kp, alice, bob) = setup();
    let executor = zero_fee_executor();

    let mut prev_nonce = 0u64;

    for i in 0..5 {
        let mut op = UserOperation {
            sender: alice,
            nonce: i,
            actions: vec![Action::Transfer {
                to: bob,
                amount: 1,
            }],
            max_fee: 100_000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op);

        executor.execute_block(&mut store, &[op]);

        let state = StateManager::new(&mut store);
        let current_nonce = state.require_account(&alice).unwrap().nonce;
        assert!(
            current_nonce >= prev_nonce,
            "nonce must never decrease: was {prev_nonce}, now {current_nonce}"
        );
        prev_nonce = current_nonce;
    }
}

// ── Test: Nonce replay after multi-action rollback ────────────

#[test]
fn nonce_not_replayable_after_multiaction_rollback() {
    let (mut store, kp, alice, bob) = setup();
    let executor = zero_fee_executor();

    // Submit a failing multi-action op with nonce=0.
    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![
            Action::Transfer { to: bob, amount: 1 },
            Action::Transfer {
                to: bob,
                amount: u128::MAX,
            },
        ],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);
    let result = executor.execute_block(&mut store, &[op.clone()]);
    assert!(!result.receipts[0].success);

    // Try to replay the exact same operation with nonce=0.
    let result2 = executor.execute_block(&mut store, &[op]);
    assert!(
        !result2.receipts[0].success,
        "replayed operation with consumed nonce must fail"
    );
    assert!(
        result2.receipts[0]
            .error
            .as_ref()
            .map(|e| e.contains("nonce"))
            .unwrap_or(false),
        "error should mention nonce"
    );
}
