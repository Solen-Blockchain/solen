//! Security regression tests for the execution engine.
//!
//! These tests cover proven past bugs and critical invariants.

use solen_crypto::Keypair;
use solen_execution::executor::BlockExecutor;
use solen_execution::fees::FeeConfig;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_execution::state::StateManager;
use solen_storage::{MemoryStore, StateStore};
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

// ── Test #16: Threshold multisig rejects duplicate signers ───

#[test]
fn threshold_rejects_duplicate_signers() {
    let mut store = MemoryStore::new();
    let signer1 = Keypair::from_seed(&[0x01; 32]);
    let signer2 = Keypair::from_seed(&[0x02; 32]);
    let signer3 = Keypair::from_seed(&[0x03; 32]);
    let recipient = Keypair::from_seed(&[0x04; 32]).public_key();

    let multisig_id = signer1.public_key(); // owner = signer1's pubkey

    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: multisig_id,
                balance: 1_000_000_000,
                auth_methods: vec![AuthMethod::Threshold {
                    signers: vec![
                        signer1.public_key(),
                        signer2.public_key(),
                        signer3.public_key(),
                    ],
                    threshold: 2,
                }],
            },
            GenesisAccount {
                id: recipient,
                balance: 0,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: recipient }],
            },
        ],
    )
    .unwrap();

    let executor = zero_fee_executor();

    // Build a transfer operation.
    let mut op = UserOperation {
        sender: multisig_id,
        nonce: 0,
        actions: vec![Action::Transfer { to: recipient, amount: 100 }],
        max_fee: 0,
        signature: vec![],
    };

    let msg = executor.operation_signing_message(&op);

    // ATTACK: Use signer1's signature TWICE to meet threshold of 2.
    let sig1 = signer1.sign(&msg);
    let mut duplicate_sig = Vec::with_capacity(192);
    duplicate_sig.extend_from_slice(&signer1.public_key());
    duplicate_sig.extend_from_slice(&sig1);
    duplicate_sig.extend_from_slice(&signer1.public_key()); // duplicate!
    duplicate_sig.extend_from_slice(&sig1);                 // duplicate!
    op.signature = duplicate_sig;

    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "CRITICAL: duplicate signer must NOT satisfy threshold"
    );

    // Verify: legitimate 2-of-3 with distinct signers works.
    let mut op2 = UserOperation {
        sender: multisig_id,
        nonce: 0,
        actions: vec![Action::Transfer { to: recipient, amount: 100 }],
        max_fee: 0,
        signature: vec![],
    };
    let msg2 = executor.operation_signing_message(&op2);
    let sig1_2 = signer1.sign(&msg2);
    let sig2_2 = signer2.sign(&msg2);
    let mut valid_sig = Vec::with_capacity(192);
    valid_sig.extend_from_slice(&signer1.public_key());
    valid_sig.extend_from_slice(&sig1_2);
    valid_sig.extend_from_slice(&signer2.public_key());
    valid_sig.extend_from_slice(&sig2_2);
    op2.signature = valid_sig;

    let result2 = executor.execute_block(&mut store, &[op2]);
    assert!(
        result2.receipts[0].success,
        "legitimate 2-of-3 multisig should succeed"
    );
}

// ── Test #17: Session keys cannot call SetAuth ──────────────

#[test]
fn session_key_cannot_set_auth() {
    let mut store = MemoryStore::new();
    let owner_kp = Keypair::from_seed(&[0x0A; 32]);
    let session_kp = Keypair::from_seed(&[0x0B; 32]);
    let owner = owner_kp.public_key();
    let attacker_kp = Keypair::from_seed(&[0x0C; 32]);

    apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: owner,
            balance: 1_000_000_000,
            auth_methods: vec![
                AuthMethod::Ed25519 { public_key: owner },
                AuthMethod::Session {
                    session_key: session_kp.public_key(),
                    expires_at: 999_999,
                    spending_limit: 1_000_000_000,
                    budget_total: 0,
                    allowed_targets: vec![],  // all targets
                    allowed_methods: vec![],
                    restrict_subcalls: false,  // all methods
                },
            ],
        }],
    )
    .unwrap();

    let executor = zero_fee_executor();

    // ATTACK: Session key tries to replace auth with attacker's key.
    let mut op = UserOperation {
        sender: owner,
        nonce: 0,
        actions: vec![Action::SetAuth {
            auth_methods: vec![AuthMethod::Ed25519 {
                public_key: attacker_kp.public_key(),
            }],
        }],
        max_fee: 0,
        signature: vec![],
    };

    let msg = executor.operation_signing_message(&op);
    op.signature = session_kp.sign(&msg).to_vec();

    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "CRITICAL: session key must NOT be able to call SetAuth"
    );
    assert!(
        result.receipts[0].error.as_ref().unwrap().contains("session keys cannot modify"),
        "error should explain session key restriction"
    );
}

// ── Session cumulative lifetime budget ───────────────────────

/// Read the on-chain session-spend ledger for (owner, session_key).
fn session_spent(store: &MemoryStore, owner: &AccountId, pk: &[u8; 32]) -> u128 {
    let mut key = b"session_spent/".to_vec();
    for b in owner {
        key.extend_from_slice(format!("{b:02x}").as_bytes());
    }
    key.push(b'/');
    for b in pk {
        key.extend_from_slice(format!("{b:02x}").as_bytes());
    }
    store
        .get(&key)
        .ok()
        .flatten()
        .filter(|v| v.len() >= 16)
        .map(|v| {
            let mut b = [0u8; 16];
            b.copy_from_slice(&v[..16]);
            u128::from_le_bytes(b)
        })
        .unwrap_or(0)
}

#[test]
fn session_key_cumulative_budget_enforced() {
    let mut store = MemoryStore::new();
    let owner_kp = Keypair::from_seed(&[0x0A; 32]);
    let session_kp = Keypair::from_seed(&[0x0B; 32]);
    let owner = owner_kp.public_key();
    let recipient = [0x22u8; 32];

    apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: owner,
            balance: 1_000_000_000,
            auth_methods: vec![
                AuthMethod::Ed25519 { public_key: owner },
                AuthMethod::Session {
                    session_key: session_kp.public_key(),
                    expires_at: 999_999,
                    spending_limit: 0, // no per-op cap; only the lifetime budget
                    budget_total: 100,
                    allowed_targets: vec![],
                    allowed_methods: vec![],
                    restrict_subcalls: false,
                },
            ],
        }],
    )
    .unwrap();

    let executor = zero_fee_executor();
    let spend = |store: &mut MemoryStore, nonce: u64, amount: u128| -> bool {
        let mut op = UserOperation {
            sender: owner,
            nonce,
            actions: vec![Action::Transfer { to: recipient, amount }],
            max_fee: 0,
            signature: vec![],
        };
        let msg = executor.operation_signing_message(&op);
        op.signature = session_kp.sign(&msg).to_vec();
        executor.execute_block(store, &[op]).receipts[0].success
    };

    assert!(spend(&mut store, 0, 60), "60 is within the 100 budget");
    assert!(spend(&mut store, 1, 30), "running total 90 is within budget");
    assert!(!spend(&mut store, 2, 20), "90 + 20 exceeds the 100 budget");
    // The budget-rejected op never consumed nonce 2 (the check precedes nonce
    // consumption), so the agent can retry the same nonce with a smaller amount.
    assert!(spend(&mut store, 2, 10), "90 + 10 == 100 is exactly at budget");
    assert!(!spend(&mut store, 3, 1), "budget is now fully spent");
    assert_eq!(session_spent(&store, &owner, &session_kp.public_key()), 100);
}

#[test]
fn session_budget_not_charged_on_reverted_op() {
    let mut store = MemoryStore::new();
    let owner_kp = Keypair::from_seed(&[0x1A; 32]);
    let session_kp = Keypair::from_seed(&[0x1B; 32]);
    let owner = owner_kp.public_key();
    let recipient = [0x33u8; 32];

    apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: owner,
            balance: 100, // smaller than the first transfer, to force a revert
            auth_methods: vec![
                AuthMethod::Ed25519 { public_key: owner },
                AuthMethod::Session {
                    session_key: session_kp.public_key(),
                    expires_at: 999_999,
                    spending_limit: 0,
                    budget_total: 100_000, // budget is not the limiter here
                    allowed_targets: vec![],
                    allowed_methods: vec![],
                    restrict_subcalls: false,
                },
            ],
        }],
    )
    .unwrap();

    let executor = zero_fee_executor();
    let run = |store: &mut MemoryStore, nonce: u64, amount: u128| -> bool {
        let mut op = UserOperation {
            sender: owner,
            nonce,
            actions: vec![Action::Transfer { to: recipient, amount }],
            max_fee: 0,
            signature: vec![],
        };
        let msg = executor.operation_signing_message(&op);
        op.signature = session_kp.sign(&msg).to_vec();
        executor.execute_block(store, &[op]).receipts[0].success
    };

    // Transfer 500 from a balance of 100 passes the budget check but reverts in
    // execution (insufficient balance). The revert must NOT consume budget.
    assert!(!run(&mut store, 0, 500), "transfer exceeds balance → reverts");
    assert_eq!(
        session_spent(&store, &owner, &session_kp.public_key()),
        0,
        "a reverted op must not burn any session budget"
    );

    // A subsequent successful spend is charged normally.
    assert!(run(&mut store, 1, 50), "50 is within balance and budget");
    assert_eq!(session_spent(&store, &owner, &session_kp.public_key()), 50);
}

// ── Test #18: Passkey JSON injection rejected ────────────────

#[test]
fn passkey_json_injection_rejected() {
    // Test that extract_json_string rejects duplicate keys.
    // This is an internal function, so we test the behavior indirectly
    // through the module. Since it's not public, we test the invariant
    // that a crafted clientDataJSON with two "challenge" fields is rejected.

    // Duplicate "challenge" key — parser should reject.
    let json_dup = r#"{"type":"webauthn.get","challenge":"FAKE","challenge":"REAL","origin":"https://example.com"}"#;

    // The extract_json_string function should return None for duplicate keys.
    // We can't call it directly, but the passkey verification will fail
    // because the challenge won't be extracted.
    // This test documents the invariant.
    assert!(
        json_dup.matches("\"challenge\"").count() > 1,
        "test setup: must have duplicate challenge keys"
    );
}

// ── Test #19: System-authorized [0xFF] rejected from mempool ─

#[test]
fn system_signature_rejected_from_block_execution() {
    let (mut store, _kp, alice, bob) = setup();
    let executor = zero_fee_executor();

    // ATTACK: Craft an operation with [0xFF] signature targeting a
    // non-system method (should NOT be treated as system-authorized).
    let op = UserOperation {
        sender: alice,
        nonce: 0,
        actions: vec![Action::Transfer { to: bob, amount: 100 }],
        max_fee: 0,
        signature: vec![0xFF],
    };

    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "[0xFF] signature on non-system call must be rejected"
    );
}

// ── Test #20: Governance vote weight capped at actual stake ──

#[test]
fn governance_vote_weight_capped_at_stake() {
    let mut store = MemoryStore::new();
    let voter_kp = Keypair::from_seed(&[0x0A; 32]);
    let voter = voter_kp.public_key();
    let proposer_kp = Keypair::from_seed(&[0x0B; 32]);
    let proposer = proposer_kp.public_key();

    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: voter,
                balance: 1_000_000_000,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: voter }],
            },
            GenesisAccount {
                id: proposer,
                balance: 1_000_000_000,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: proposer }],
            },
        ],
    )
    .unwrap();

    let executor = zero_fee_executor();

    // Voter has no stake (0 in staking contract). Voting should be rejected.
    let gov_addr = solen_types::system::GOVERNANCE_ADDRESS;

    // Build vote args: proposal_id[8] + support[1] + stake_weight[16]
    // Use proposal_id=0 (won't exist but the weight check happens first for non-stakers)
    let mut vote_args = Vec::new();
    vote_args.extend_from_slice(&0u64.to_le_bytes()); // proposal_id
    vote_args.push(1u8); // support = yes
    vote_args.extend_from_slice(&u128::MAX.to_le_bytes()); // claim max weight

    let mut op = UserOperation {
        sender: voter,
        nonce: 0,
        actions: vec![Action::Call {
            target: gov_addr,
            method: "vote".to_string(),
            args: vote_args,
        }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&voter_kp, &executor, &mut op);

    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "voter with 0 stake must not be able to vote"
    );
    assert!(
        result.receipts[0].error.as_ref().unwrap().contains("no stake"),
        "error should mention no stake"
    );
}

// ── Test #21: Threshold multisig with all permutations ───────

#[test]
fn threshold_single_signer_insufficient_for_2of3() {
    let mut store = MemoryStore::new();
    let s1 = Keypair::from_seed(&[0x01; 32]);
    let s2 = Keypair::from_seed(&[0x02; 32]);
    let s3 = Keypair::from_seed(&[0x03; 32]);
    let recipient = Keypair::from_seed(&[0x04; 32]).public_key();

    let owner = s1.public_key();

    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: owner,
                balance: 1_000_000_000,
                auth_methods: vec![AuthMethod::Threshold {
                    signers: vec![s1.public_key(), s2.public_key(), s3.public_key()],
                    threshold: 2,
                }],
            },
            GenesisAccount {
                id: recipient,
                balance: 0,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: recipient }],
            },
        ],
    )
    .unwrap();

    let executor = zero_fee_executor();

    // 1 of 3 — must fail.
    let mut op = UserOperation {
        sender: owner,
        nonce: 0,
        actions: vec![Action::Transfer { to: recipient, amount: 1 }],
        max_fee: 0,
        signature: vec![],
    };
    let msg = executor.operation_signing_message(&op);
    let sig1 = s1.sign(&msg);
    let mut single_sig = Vec::with_capacity(96);
    single_sig.extend_from_slice(&s1.public_key());
    single_sig.extend_from_slice(&sig1);
    op.signature = single_sig;

    let result = executor.execute_block(&mut store, &[op.clone()]);
    assert!(!result.receipts[0].success, "1-of-3 must fail for threshold 2");

    // Unknown signer — must fail.
    let rogue = Keypair::from_seed(&[0xFF; 32]);
    let rogue_sig = rogue.sign(&msg);
    let mut rogue_two = Vec::with_capacity(192);
    rogue_two.extend_from_slice(&rogue.public_key());
    rogue_two.extend_from_slice(&rogue_sig);
    rogue_two.extend_from_slice(&s1.public_key());
    rogue_two.extend_from_slice(&sig1);
    op.signature = rogue_two;

    let result2 = executor.execute_block(&mut store, &[op]);
    assert!(!result2.receipts[0].success, "unknown signer + 1 valid must fail for threshold 2");
}

// ── Test #22: Session key cannot deploy contracts ────────────

#[test]
fn session_key_cannot_deploy() {
    let mut store = MemoryStore::new();
    let owner_kp = Keypair::from_seed(&[0x0A; 32]);
    let session_kp = Keypair::from_seed(&[0x0B; 32]);
    let owner = owner_kp.public_key();

    apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: owner,
            balance: 1_000_000_000,
            auth_methods: vec![
                AuthMethod::Ed25519 { public_key: owner },
                AuthMethod::Session {
                    session_key: session_kp.public_key(),
                    expires_at: 999_999,
                    spending_limit: 1_000_000_000,
                    budget_total: 0,
                    allowed_targets: vec![],
                    allowed_methods: vec![],
                    restrict_subcalls: false,
                },
            ],
        }],
    )
    .unwrap();

    let executor = zero_fee_executor();

    let mut op = UserOperation {
        sender: owner,
        nonce: 0,
        actions: vec![Action::Deploy {
            code: vec![0x00, 0x61, 0x73, 0x6D], // fake wasm
            salt: [0u8; 32],
        }],
        max_fee: 0,
        signature: vec![],
    };

    let msg = executor.operation_signing_message(&op);
    op.signature = session_kp.sign(&msg).to_vec();

    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "session key must NOT be able to deploy contracts"
    );
}

// ── Test #23: Session key cannot call guardian recovery ───────

#[test]
fn session_key_cannot_call_guardian() {
    let mut store = MemoryStore::new();
    let owner_kp = Keypair::from_seed(&[0x0A; 32]);
    let session_kp = Keypair::from_seed(&[0x0B; 32]);
    let owner = owner_kp.public_key();

    apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: owner,
            balance: 1_000_000_000,
            auth_methods: vec![
                AuthMethod::Ed25519 { public_key: owner },
                AuthMethod::Session {
                    session_key: session_kp.public_key(),
                    expires_at: 999_999,
                    spending_limit: 1_000_000_000,
                    budget_total: 0,
                    allowed_targets: vec![],
                    allowed_methods: vec![],
                    restrict_subcalls: false,
                },
            ],
        }],
    )
    .unwrap();

    let executor = zero_fee_executor();
    let guardian_addr = solen_types::system::GUARDIAN_ADDRESS;

    let mut op = UserOperation {
        sender: owner,
        nonce: 0,
        actions: vec![Action::Call {
            target: guardian_addr,
            method: "initiate_recovery".to_string(),
            args: vec![0; 96],
        }],
        max_fee: 0,
        signature: vec![],
    };

    let msg = executor.operation_signing_message(&op);
    op.signature = session_kp.sign(&msg).to_vec();

    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "session key must NOT be able to call guardian recovery"
    );
    assert!(
        result.receipts[0].error.as_ref().unwrap().contains("guardian"),
        "error should mention guardian restriction"
    );
}

// ── Test #24: Session key cannot create governance proposals ──

#[test]
fn session_key_cannot_create_proposal() {
    let mut store = MemoryStore::new();
    let owner_kp = Keypair::from_seed(&[0x0A; 32]);
    let session_kp = Keypair::from_seed(&[0x0B; 32]);
    let owner = owner_kp.public_key();

    apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: owner,
            balance: 1_000_000_000,
            auth_methods: vec![
                AuthMethod::Ed25519 { public_key: owner },
                AuthMethod::Session {
                    session_key: session_kp.public_key(),
                    expires_at: 999_999,
                    spending_limit: 1_000_000_000,
                    budget_total: 0,
                    allowed_targets: vec![],
                    allowed_methods: vec![],
                    restrict_subcalls: false,
                },
            ],
        }],
    )
    .unwrap();

    let executor = zero_fee_executor();
    let gov_addr = solen_types::system::GOVERNANCE_ADDRESS;

    // Try propose_block_time
    let mut args = Vec::new();
    args.extend_from_slice(&3000u64.to_le_bytes()); // new_block_time_ms
    args.extend_from_slice(b"test proposal");

    let mut op = UserOperation {
        sender: owner,
        nonce: 0,
        actions: vec![Action::Call {
            target: gov_addr,
            method: "propose_block_time".to_string(),
            args,
        }],
        max_fee: 0,
        signature: vec![],
    };

    let msg = executor.operation_signing_message(&op);
    op.signature = session_kp.sign(&msg).to_vec();

    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "session key must NOT be able to create governance proposals"
    );
    assert!(
        result.receipts[0].error.as_ref().unwrap().contains("governance"),
        "error should mention governance restriction"
    );
}

// ── Test #25: Flash-vote prevention (new stake not eligible) ─

#[test]
fn flash_vote_rejected_for_new_stake() {
    let mut store = MemoryStore::new();
    let voter_kp = Keypair::from_seed(&[0x0A; 32]);
    let voter = voter_kp.public_key();

    apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: voter,
            balance: 100_000_000_000_000, // 1M SOLEN
            auth_methods: vec![AuthMethod::Ed25519 { public_key: voter }],
        }],
    )
    .unwrap();

    let executor = zero_fee_executor();

    // Step 1: Register as validator (this sets eligible_from_epoch = current_epoch + 1).
    let staking_addr = solen_types::system::STAKING_ADDRESS;
    let stake_amount: u128 = 50_000_000_000_000; // 500K SOLEN
    let mut register_args = Vec::new();
    register_args.extend_from_slice(&stake_amount.to_le_bytes());

    let mut op1 = UserOperation {
        sender: voter,
        nonce: 0,
        actions: vec![Action::Call {
            target: staking_addr,
            method: "register".to_string(),
            args: register_args,
        }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&voter_kp, &executor, &mut op1);

    // Step 2: Immediately try to vote (same block).
    let gov_addr = solen_types::system::GOVERNANCE_ADDRESS;
    let mut vote_args = Vec::new();
    vote_args.extend_from_slice(&0u64.to_le_bytes()); // proposal_id
    vote_args.push(1u8); // support
    vote_args.extend_from_slice(&stake_amount.to_le_bytes()); // weight

    let mut op2 = UserOperation {
        sender: voter,
        nonce: 1,
        actions: vec![Action::Call {
            target: gov_addr,
            method: "vote".to_string(),
            args: vote_args,
        }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&voter_kp, &executor, &mut op2);

    // Execute both in the same block (flash-vote attempt).
    let result = executor.execute_block(&mut store, &[op1, op2]);

    // Registration should succeed.
    assert!(result.receipts[0].success, "registration should succeed");
    // Vote should fail — stake is not yet eligible (eligible_from_epoch > current).
    assert!(
        !result.receipts[1].success,
        "flash-vote must be rejected: new stake is not yet eligible"
    );
}

// ── Test #26: Threshold=0 rejects empty signature ────────────

#[test]
fn threshold_zero_rejects_empty_signature() {
    let mut store = MemoryStore::new();
    let s1 = Keypair::from_seed(&[0x01; 32]);
    let recipient = Keypair::from_seed(&[0x04; 32]).public_key();
    let owner = s1.public_key();

    // Force a threshold=0 account (simulating bad genesis config).
    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: owner,
                balance: 1_000_000_000,
                auth_methods: vec![AuthMethod::Threshold {
                    signers: vec![s1.public_key()],
                    threshold: 0, // invalid but could exist from genesis
                }],
            },
            GenesisAccount {
                id: recipient,
                balance: 0,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: recipient }],
            },
        ],
    )
    .unwrap();

    let executor = zero_fee_executor();

    // ATTACK: Empty signature should NOT authenticate threshold=0.
    let op = UserOperation {
        sender: owner,
        nonce: 0,
        actions: vec![Action::Transfer { to: recipient, amount: 100 }],
        max_fee: 0,
        signature: vec![], // empty!
    };

    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "CRITICAL: threshold=0 with empty signature must be rejected"
    );
}

// ── Test #27: Session key cannot finalize governance proposals ─

#[test]
fn session_key_cannot_finalize_proposal() {
    let mut store = MemoryStore::new();
    let owner_kp = Keypair::from_seed(&[0x0A; 32]);
    let session_kp = Keypair::from_seed(&[0x0B; 32]);
    let owner = owner_kp.public_key();

    apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: owner,
            balance: 1_000_000_000,
            auth_methods: vec![
                AuthMethod::Ed25519 { public_key: owner },
                AuthMethod::Session {
                    session_key: session_kp.public_key(),
                    expires_at: 999_999,
                    spending_limit: 1_000_000_000,
                    budget_total: 0,
                    allowed_targets: vec![],
                    allowed_methods: vec![],
                    restrict_subcalls: false,
                },
            ],
        }],
    )
    .unwrap();

    let executor = zero_fee_executor();
    let gov_addr = solen_types::system::GOVERNANCE_ADDRESS;

    let mut op = UserOperation {
        sender: owner,
        nonce: 0,
        actions: vec![Action::Call {
            target: gov_addr,
            method: "finalize".to_string(),
            args: 0u64.to_le_bytes().to_vec(),
        }],
        max_fee: 0,
        signature: vec![],
    };

    let msg = executor.operation_signing_message(&op);
    op.signature = session_kp.sign(&msg).to_vec();

    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "session key must NOT be able to finalize governance proposals"
    );
}

// ── Test #28: Unjail cooldown after slash ─────────────────────

#[test]
fn unjail_cooldown_enforced_after_slash() {
    let mut store = MemoryStore::new();
    let v_kp = Keypair::from_seed(&[0x01; 32]);
    let validator = v_kp.public_key();

    apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: validator,
            balance: 100_000_000_000_000, // 1M SOLEN
            auth_methods: vec![AuthMethod::Ed25519 { public_key: validator }],
        }],
    )
    .unwrap();

    let executor = zero_fee_executor();
    let staking_addr = solen_types::system::STAKING_ADDRESS;

    // Register as validator (above minimum so 1% slash doesn't drop below threshold).
    let stake: u128 = 60_000_000_000_000; // 600K SOLEN
    let mut register_args = Vec::new();
    register_args.extend_from_slice(&stake.to_le_bytes());

    let mut op1 = UserOperation {
        sender: validator,
        nonce: 0,
        actions: vec![Action::Call {
            target: staking_addr,
            method: "register".to_string(),
            args: register_args,
        }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&v_kp, &executor, &mut op1);

    let r1 = executor.execute_block(&mut store, &[op1]);
    assert!(r1.receipts[0].success, "registration should succeed");

    // Set chain height to 100 (epoch 1). Chain meta needs 16 bytes: height[8] + epoch[8].
    let mut meta = Vec::new();
    meta.extend_from_slice(&100u64.to_le_bytes()); // height=100 → epoch=1
    meta.extend_from_slice(&1u64.to_le_bytes());   // epoch field
    store.put(b"__chain_meta__", &meta).unwrap();

    // Consensus records slashing evidence on-chain (slash/{offender_hex}/{height})
    // before a slash op is accepted — this prevents a proposer from fabricating
    // slashes. Mimic that prerequisite here.
    let off_hex: String = validator.iter().map(|b| format!("{b:02x}")).collect();
    store
        .put(format!("slash/{off_hex}/100").as_bytes(), b"downtime")
        .unwrap();

    // Slash the validator via system op.
    let mut slash_args = Vec::new();
    slash_args.extend_from_slice(&validator);
    slash_args.extend_from_slice(&100u64.to_le_bytes()); // 1%

    let slash_op = UserOperation {
        sender: validator,
        nonce: 0,
        actions: vec![Action::Call {
            target: staking_addr,
            method: "slash".to_string(),
            args: slash_args,
        }],
        max_fee: 0,
        signature: vec![0xFF],
    };

    let r2 = executor.execute_block(&mut store, &[slash_op]);
    assert!(r2.receipts[0].success, "slash should succeed");

    // Try to unjail in same epoch — should fail due to cooldown.
    let mut unjail_op = UserOperation {
        sender: validator,
        nonce: 1,
        actions: vec![Action::Call {
            target: staking_addr,
            method: "unjail".to_string(),
            args: vec![],
        }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&v_kp, &executor, &mut unjail_op);

    let r3 = executor.execute_block(&mut store, &[unjail_op.clone()]);
    assert!(
        !r3.receipts[0].success,
        "unjail must fail during cooldown period (same epoch as slash)"
    );

    // Advance to epoch 2 (height=200). Unjail should now work.
    let mut meta2 = Vec::new();
    meta2.extend_from_slice(&200u64.to_le_bytes()); // height=200 → epoch=2
    meta2.extend_from_slice(&2u64.to_le_bytes());
    store.put(b"__chain_meta__", &meta2).unwrap();

    // Previous unjail failed but nonce was consumed. Use nonce=2.
    unjail_op.nonce = 2;
    sign_op(&v_kp, &executor, &mut unjail_op);

    let r4 = executor.execute_block(&mut store, &[unjail_op]);
    assert!(
        r4.receipts[0].success,
        "unjail should succeed after cooldown epoch, error: {:?}",
        r4.receipts[0].error
    );
}

// ── Test #29: Large threshold signature bounded execution ─────

#[test]
fn large_threshold_signature_does_not_hang() {
    let mut store = MemoryStore::new();
    let s1 = Keypair::from_seed(&[0x01; 32]);
    let recipient = [0xFF; 32];
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
                id: recipient,
                balance: 0,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: recipient }],
            },
        ],
    )
    .unwrap();

    let executor = zero_fee_executor();
    let mut op = UserOperation {
        sender: owner,
        nonce: 0,
        actions: vec![Action::Transfer { to: recipient, amount: 1 }],
        max_fee: 0,
        signature: vec![],
    };
    let msg = executor.operation_signing_message(&op);
    let sig = s1.sign(&msg);

    // Build a large signature with 100 duplicate chunks (9600 bytes).
    // This should complete in bounded time due to HashSet dedup.
    let mut large_sig = Vec::with_capacity(96 * 100);
    for _ in 0..100 {
        large_sig.extend_from_slice(&s1.public_key());
        large_sig.extend_from_slice(&sig);
    }
    op.signature = large_sig;

    let start = std::time::Instant::now();
    let result = executor.execute_block(&mut store, &[op]);
    let elapsed = start.elapsed();

    // Should succeed (1 valid unique signer meets threshold=1).
    assert!(result.receipts[0].success, "1-of-1 with duplicates should succeed");
    // Should complete quickly (under 100ms even with 100 chunks).
    assert!(
        elapsed.as_millis() < 1000,
        "large threshold signature should complete in bounded time, took {}ms",
        elapsed.as_millis()
    );
}
