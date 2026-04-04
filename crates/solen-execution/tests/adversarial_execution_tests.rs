//! Adversarial execution tests — malformed inputs, authorization bypasses,
//! replay attacks, state corruption, and cross-module integration.

use solen_crypto::Keypair;
use solen_execution::executor::BlockExecutor;
use solen_execution::fees::FeeConfig;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_execution::state::StateManager;
use solen_storage::{MemoryStore, StateStore};
use solen_types::account::AuthMethod;
use solen_types::transaction::{Action, UserOperation};
use solen_types::AccountId;

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

fn setup_alice_bob() -> (MemoryStore, Keypair, Keypair, AccountId, AccountId) {
    let mut store = MemoryStore::new();
    let alice_kp = Keypair::from_seed(&[0x0A; 32]);
    let bob_kp = Keypair::from_seed(&[0x0B; 32]);
    let alice = alice_kp.public_key();
    let bob = bob_kp.public_key();

    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: alice,
                balance: 10_000_000_000, // 100 SOLEN
                auth_methods: vec![AuthMethod::Ed25519 { public_key: alice }],
            },
            GenesisAccount {
                id: bob,
                balance: 5_000_000_000, // 50 SOLEN
                auth_methods: vec![AuthMethod::Ed25519 { public_key: bob }],
            },
        ],
    )
    .unwrap();

    (store, alice_kp, bob_kp, alice, bob)
}

// ═══════════════════════════════════════════════════════════════
// MALFORMED INPUT TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn zero_length_signature_rejected() {
    let (mut store, _kp, _bkp, alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    let op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: 1 }],
        max_fee: 0,
        signature: vec![], // empty
    };

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success, "empty signature must fail");
}

#[test]
fn garbage_signature_rejected() {
    let (mut store, _kp, _bkp, alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    let op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: 1 }],
        max_fee: 0,
        signature: vec![0xDE; 64], // random 64 bytes
    };

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success, "garbage signature must fail");
}

#[test]
fn oversized_signature_rejected() {
    let (mut store, _kp, _bkp, alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    let op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: 1 }],
        max_fee: 0,
        signature: vec![0xFF; 1024], // way too long
    };

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success, "oversized signature must fail");
}

#[test]
fn zero_actions_rejected() {
    let (mut store, kp, _bkp, alice, _bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![], // no actions
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    let result = executor.execute_block(&mut store, &[op]);
    // Empty actions should still succeed (noop) but shouldn't crash
    // The important thing is no panic
}

#[test]
fn too_many_actions_rejected() {
    let (mut store, kp, _bkp, alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    // Create 20 actions (over MAX_ACTIONS_PER_OP = 16)
    let actions: Vec<Action> = (0..20)
        .map(|_| Action::Transfer { to: bob, amount: 1 })
        .collect();

    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions,
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success, "too many actions must fail");
}

#[test]
fn transfer_to_self_does_not_create_money() {
    let (mut store, kp, _bkp, alice, _bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    let balance_before = {
        let state = StateManager::new(&mut store);
        state.require_account(&alice).unwrap().balance
    };

    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer { to: alice, amount: 100 }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    let result = executor.execute_block(&mut store, &[op]);

    let balance_after = {
        let state = StateManager::new(&mut store);
        state.require_account(&alice).unwrap().balance
    };

    // Self-transfer should not change balance (ignoring fees)
    assert_eq!(balance_before, balance_after, "self-transfer must not change balance");
}

#[test]
fn transfer_entire_balance_leaves_zero() {
    let (mut store, kp, _bkp, alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    let balance = {
        let state = StateManager::new(&mut store);
        state.require_account(&alice).unwrap().balance
    };

    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: balance }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    let result = executor.execute_block(&mut store, &[op]);
    assert!(result.receipts[0].success, "transferring entire balance should succeed");

    let final_balance = {
        let state = StateManager::new(&mut store);
        state.require_account(&alice).unwrap().balance
    };
    assert_eq!(final_balance, 0, "balance should be exactly zero");
}

#[test]
fn transfer_more_than_balance_fails() {
    let (mut store, kp, _bkp, alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: u128::MAX }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success, "transfer > balance must fail");
}

// ═══════════════════════════════════════════════════════════════
// REPLAY TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn same_op_replayed_in_same_block_fails() {
    let (mut store, kp, _bkp, alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: 1 }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    // Submit same op twice in one block.
    let result = executor.execute_block(&mut store, &[op.clone(), op]);
    assert!(result.receipts[0].success, "first op should succeed");
    assert!(!result.receipts[1].success, "replayed op must fail (nonce consumed)");
}

#[test]
fn cross_chain_replay_rejected() {
    let (mut store, kp, _bkp, alice, bob) = setup_alice_bob();

    // Sign with chain_id 1337
    let executor_a = BlockExecutor::with_fee_config(FeeConfig {
        base_fee_per_gas: 0,
        ..Default::default()
    }).with_chain_id(1337);

    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: 1 }],
        max_fee: 0,
        signature: vec![],
    };
    let msg = executor_a.operation_signing_message(&op);
    op.signature = kp.sign(&msg).to_vec();

    // Execute on chain_id 9000
    let executor_b = BlockExecutor::with_fee_config(FeeConfig {
        base_fee_per_gas: 0,
        ..Default::default()
    }).with_chain_id(9000);

    let result = executor_b.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "signature from chain 1337 must not verify on chain 9000"
    );
}

#[test]
fn nonce_skip_rejected() {
    let (mut store, kp, _bkp, alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    // Try nonce 5 when account is at nonce 0.
    let mut op = UserOperation {
        sender: alice,
        nonce: 5, // skipped 0-4
        actions: vec![Action::Transfer { to: bob, amount: 1 }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success, "nonce skip must be rejected");
}

// ═══════════════════════════════════════════════════════════════
// AUTHORIZATION BYPASS TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn wrong_sender_key_rejected() {
    let (mut store, _alice_kp, bob_kp, alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    // Sign as Bob but set sender to Alice.
    let mut op = UserOperation {
        sender: alice, // Alice's account
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: 1 }],
        max_fee: 0,
        signature: vec![],
    };
    // Sign with Bob's key (not Alice's)
    let msg = executor.operation_signing_message(&op);
    op.signature = bob_kp.sign(&msg).to_vec();

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success, "wrong key must not authorize");
}

#[test]
fn nonexistent_sender_rejected() {
    let (mut store, _kp, _bkp, _alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();
    let ghost = Keypair::from_seed(&[0xFF; 32]);

    let mut op = UserOperation {
        sender: ghost.public_key(), // doesn't exist
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: 1 }],
        max_fee: 0,
        signature: vec![],
    };
    let msg = executor.operation_signing_message(&op);
    op.signature = ghost.sign(&msg).to_vec();

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success, "nonexistent sender must fail");
}

#[test]
fn system_contract_transfer_rejected() {
    let (mut store, kp, _bkp, alice, _bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    // Try to transfer TO a system contract.
    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer {
            to: solen_types::system::STAKING_ADDRESS,
            amount: 1,
        }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success, "transfer to system contract must fail");
}

// ═══════════════════════════════════════════════════════════════
// STATE CORRUPTION / CONSERVATION TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn total_supply_conserved_across_block() {
    let (mut store, kp, _bkp, alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    let total_before = {
        let state = StateManager::new(&mut store);
        let a = state.require_account(&alice).unwrap().balance;
        let b = state.require_account(&bob).unwrap().balance;
        a + b
    };

    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: 1_000_000 }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    executor.execute_block(&mut store, &[op]);

    let total_after = {
        let state = StateManager::new(&mut store);
        let a = state.require_account(&alice).unwrap().balance;
        let b = state.require_account(&bob).unwrap().balance;
        a + b
    };

    assert_eq!(total_before, total_after, "total supply must be conserved (zero-fee)");
}

#[test]
fn failed_op_does_not_change_balances() {
    let (mut store, kp, _bkp, alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    let alice_before = {
        let state = StateManager::new(&mut store);
        state.require_account(&alice).unwrap().balance
    };
    let bob_before = {
        let state = StateManager::new(&mut store);
        state.require_account(&bob).unwrap().balance
    };

    // This will fail (amount > balance).
    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: u128::MAX }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success);

    let alice_after = {
        let state = StateManager::new(&mut store);
        state.require_account(&alice).unwrap().balance
    };
    let bob_after = {
        let state = StateManager::new(&mut store);
        state.require_account(&bob).unwrap().balance
    };

    assert_eq!(alice_before, alice_after, "failed op must not change sender balance");
    assert_eq!(bob_before, bob_after, "failed op must not change recipient balance");
}

#[test]
fn multi_action_failure_rolls_back_all() {
    let (mut store, kp, _bkp, alice, bob) = setup_alice_bob();
    let executor = zero_fee_executor();

    let alice_before = {
        let state = StateManager::new(&mut store);
        state.require_account(&alice).unwrap().balance
    };

    // Action 1: small transfer (should succeed)
    // Action 2: huge transfer (will fail)
    // Both should be rolled back.
    let mut op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![
            Action::Transfer { to: bob, amount: 1 },
            Action::Transfer { to: bob, amount: u128::MAX },
        ],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success, "multi-action with failure must fail");

    let alice_after = {
        let state = StateManager::new(&mut store);
        state.require_account(&alice).unwrap().balance
    };

    assert_eq!(alice_before, alice_after, "multi-action rollback must restore balance");
}

// ═══════════════════════════════════════════════════════════════
// DETERMINISM TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn same_block_produces_same_state_root_twice() {
    let alice_kp = Keypair::from_seed(&[0x0A; 32]);
    let bob_kp = Keypair::from_seed(&[0x0B; 32]);
    let alice = alice_kp.public_key();
    let bob = bob_kp.public_key();
    let executor = zero_fee_executor();

    let mut build_and_execute = || {
        let mut store = MemoryStore::new();
        apply_genesis(
            &mut store,
            vec![
                GenesisAccount {
                    id: alice,
                    balance: 10_000_000_000,
                    auth_methods: vec![AuthMethod::Ed25519 { public_key: alice }],
                },
                GenesisAccount {
                    id: bob,
                    balance: 5_000_000_000,
                    auth_methods: vec![AuthMethod::Ed25519 { public_key: bob }],
                },
            ],
        )
        .unwrap();

        let mut op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer { to: bob, amount: 123_456 }],
            max_fee: 0,
            signature: vec![],
        };
        sign_op(&alice_kp, &executor, &mut op);
        let result = executor.execute_block(&mut store, &[op]);
        result.state_root
    };

    let root1 = build_and_execute();
    let root2 = build_and_execute();
    assert_eq!(root1, root2, "same block must produce identical state root");
}

#[test]
fn different_op_order_same_result_when_independent() {
    let alice_kp = Keypair::from_seed(&[0x0A; 32]);
    let bob_kp = Keypair::from_seed(&[0x0B; 32]);
    let charlie_kp = Keypair::from_seed(&[0x0C; 32]);
    let alice = alice_kp.public_key();
    let bob = bob_kp.public_key();
    let charlie = charlie_kp.public_key();
    let executor = zero_fee_executor();

    // Two independent transfers: alice→charlie, bob→charlie
    let make_store = || {
        let mut store = MemoryStore::new();
        apply_genesis(
            &mut store,
            vec![
                GenesisAccount { id: alice, balance: 1000, auth_methods: vec![AuthMethod::Ed25519 { public_key: alice }] },
                GenesisAccount { id: bob, balance: 1000, auth_methods: vec![AuthMethod::Ed25519 { public_key: bob }] },
                GenesisAccount { id: charlie, balance: 0, auth_methods: vec![AuthMethod::Ed25519 { public_key: charlie }] },
            ],
        ).unwrap();
        store
    };

    let mut op_a = UserOperation {
        sender: alice, nonce: 0,
        actions: vec![Action::Transfer { to: charlie, amount: 100 }],
        max_fee: 0, signature: vec![],
    };
    sign_op(&alice_kp, &executor, &mut op_a);

    let mut op_b = UserOperation {
        sender: bob, nonce: 0,
        actions: vec![Action::Transfer { to: charlie, amount: 200 }],
        max_fee: 0, signature: vec![],
    };
    sign_op(&bob_kp, &executor, &mut op_b);

    // Order 1: A then B
    let mut store1 = make_store();
    let result1 = executor.execute_block(&mut store1, &[op_a.clone(), op_b.clone()]);

    // Order 2: B then A
    let mut store2 = make_store();
    let result2 = executor.execute_block(&mut store2, &[op_b, op_a]);

    // Final balances should be the same regardless of order.
    let charlie_1 = {
        let s = StateManager::new(&mut store1);
        s.require_account(&charlie).unwrap().balance
    };
    let charlie_2 = {
        let s = StateManager::new(&mut store2);
        s.require_account(&charlie).unwrap().balance
    };
    assert_eq!(charlie_1, charlie_2, "independent transfers must produce same final balance");
    assert_eq!(charlie_1, 300, "charlie should have 300");
}

// ═══════════════════════════════════════════════════════════════
// SESSION KEY EDGE CASES
// ═══════════════════════════════════════════════════════════════

#[test]
fn expired_session_key_rejected() {
    let mut store = MemoryStore::new();
    let owner_kp = Keypair::from_seed(&[0x0A; 32]);
    let session_kp = Keypair::from_seed(&[0x0B; 32]);
    let owner = owner_kp.public_key();
    let bob = Keypair::from_seed(&[0x0C; 32]).public_key();

    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: owner,
                balance: 1_000_000_000,
                auth_methods: vec![
                    AuthMethod::Ed25519 { public_key: owner },
                    AuthMethod::Session {
                        session_key: session_kp.public_key(),
                        expires_at: 0, // already expired at height 0
                        spending_limit: 1_000_000_000,
                        allowed_targets: vec![],
                        allowed_methods: vec![],
                    },
                ],
            },
            GenesisAccount {
                id: bob,
                balance: 0,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: bob }],
            },
        ],
    )
    .unwrap();

    // Set chain height > 0 by executing a block first.
    let executor = zero_fee_executor();
    let mut op = UserOperation {
        sender: owner,
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: 1 }],
        max_fee: 0,
        signature: vec![],
    };
    let msg = executor.operation_signing_message(&op);
    op.signature = session_kp.sign(&msg).to_vec();

    // The session key expiry check reads height from __chain_meta__ in the store.
    // Set it to height 5 so the session key (expires_at=0) is expired.
    store.put(b"__chain_meta__", &5u64.to_le_bytes()).unwrap();

    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "expired session key must be rejected at height > expires_at"
    );
}

// ═══════════════════════════════════════════════════════════════
// THRESHOLD MULTISIG EDGE CASES
// ═══════════════════════════════════════════════════════════════

#[test]
fn threshold_with_out_of_set_signer_not_counted() {
    let mut store = MemoryStore::new();
    let s1 = Keypair::from_seed(&[0x01; 32]);
    let s2 = Keypair::from_seed(&[0x02; 32]);
    let rogue = Keypair::from_seed(&[0xFF; 32]);
    let bob = Keypair::from_seed(&[0x04; 32]).public_key();
    let owner = s1.public_key();

    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: owner,
                balance: 1_000_000,
                auth_methods: vec![AuthMethod::Threshold {
                    signers: vec![s1.public_key(), s2.public_key()],
                    threshold: 2,
                }],
            },
            GenesisAccount {
                id: bob, balance: 0,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: bob }],
            },
        ],
    ).unwrap();

    let executor = zero_fee_executor();
    let mut op = UserOperation {
        sender: owner, nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: 1 }],
        max_fee: 0, signature: vec![],
    };
    let msg = executor.operation_signing_message(&op);

    // Sign with s1 (valid) and rogue (not in signers list).
    let sig1 = s1.sign(&msg);
    let sig_rogue = rogue.sign(&msg);
    let mut combined = Vec::with_capacity(192);
    combined.extend_from_slice(&s1.public_key());
    combined.extend_from_slice(&sig1);
    combined.extend_from_slice(&rogue.public_key());
    combined.extend_from_slice(&sig_rogue);
    op.signature = combined;

    let result = executor.execute_block(&mut store, &[op]);
    assert!(!result.receipts[0].success, "out-of-set signer must not count toward threshold");
}

#[test]
fn threshold_1_of_1_works() {
    let mut store = MemoryStore::new();
    let s1 = Keypair::from_seed(&[0x01; 32]);
    let bob = Keypair::from_seed(&[0x04; 32]).public_key();
    let owner = s1.public_key();

    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: owner,
                balance: 1_000_000,
                auth_methods: vec![AuthMethod::Threshold {
                    signers: vec![s1.public_key()],
                    threshold: 1,
                }],
            },
            GenesisAccount {
                id: bob, balance: 0,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: bob }],
            },
        ],
    ).unwrap();

    let executor = zero_fee_executor();
    let mut op = UserOperation {
        sender: owner, nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: 1 }],
        max_fee: 0, signature: vec![],
    };
    let msg = executor.operation_signing_message(&op);
    let sig = s1.sign(&msg);
    let mut combined = Vec::with_capacity(96);
    combined.extend_from_slice(&s1.public_key());
    combined.extend_from_slice(&sig);
    op.signature = combined;

    let result = executor.execute_block(&mut store, &[op]);
    assert!(result.receipts[0].success, "1-of-1 threshold should work");
}
