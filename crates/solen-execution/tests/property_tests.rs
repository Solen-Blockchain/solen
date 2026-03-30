//! Property-based tests for execution invariants.

use proptest::prelude::*;
use solen_crypto::Keypair;
use solen_execution::executor::BlockExecutor;
use solen_execution::fees::FeeConfig;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_execution::state::StateManager;
use solen_storage::MemoryStore;
use solen_types::account::AuthMethod;
use solen_types::transaction::{Action, UserOperation};
use solen_types::AccountId;

fn make_id(n: u8) -> AccountId {
    let mut id = [0u8; 32];
    id[0] = n;
    id
}

fn treasury_id() -> AccountId {
    solen_execution::fees::TREASURY_ADDRESS
}

fn setup_with_balances(
    alice_bal: u128,
    bob_bal: u128,
) -> (MemoryStore, Keypair, AccountId, AccountId) {
    let mut store = MemoryStore::new();
    let kp = Keypair::generate();

    let alice = make_id(1);
    let bob = make_id(2);

    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: alice,
                balance: alice_bal,
                auth_methods: vec![AuthMethod::Ed25519 {
                    public_key: kp.public_key(),
                }],
            },
            GenesisAccount {
                id: bob,
                balance: bob_bal,
                auth_methods: vec![],
            },
            GenesisAccount {
                id: treasury_id(),
                balance: 0,
                auth_methods: vec![],
            },
        ],
    )
    .unwrap();

    (store, kp, alice, bob)
}

fn sign(kp: &Keypair, executor: &BlockExecutor, op: &mut UserOperation) {
    let msg = executor.operation_signing_message(op);
    op.signature = kp.sign(&msg).to_vec();
}

proptest! {
    /// Total token supply is conserved across transfers (with fees).
    #[test]
    fn supply_conservation(
        alice_bal in 1000u128..1_000_000,
        bob_bal in 0u128..1_000_000,
        transfer_amount in 1u128..500,
        fee_per_gas in 0u128..10,
    ) {
        let (mut store, kp, alice, bob) = setup_with_balances(alice_bal, bob_bal);
        let executor = BlockExecutor::with_fee_config(FeeConfig {
            base_fee_per_gas: fee_per_gas,
            burn_rate_bps: 0, // no burn so we can track all tokens
            ..Default::default()
        });

        let total_before = alice_bal + bob_bal;

        let mut op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer {
                to: bob,
                amount: transfer_amount.min(alice_bal),
            }],
            max_fee: 1_000_000,
            signature: vec![],
        };
        sign(&kp, &executor, &mut op);

        executor.execute_block(&mut store, &[op]);

        let state = StateManager::new(&mut store);
        let alice_after = state.get_balance(&alice).unwrap();
        let bob_after = state.get_balance(&bob).unwrap();
        let treasury_after = state.get_balance(&treasury_id()).unwrap();

        // Total supply = alice + bob + treasury (no burn)
        let total_after = alice_after + bob_after + treasury_after;
        prop_assert_eq!(total_before, total_after);
    }

    /// Nonces are strictly monotonic after successful operations.
    #[test]
    fn nonce_monotonicity(
        num_ops in 1usize..10,
    ) {
        let (mut store, kp, alice, bob) = setup_with_balances(1_000_000, 0);
        let executor = BlockExecutor::with_fee_config(FeeConfig {
            base_fee_per_gas: 0,
            ..Default::default()
        });

        let mut ops = Vec::new();
        for i in 0..num_ops {
            let mut op = UserOperation {
                sender: alice,
                nonce: i as u64,
                actions: vec![Action::Transfer { to: bob, amount: 1 }],
                max_fee: 1000,
                signature: vec![],
            };
            sign(&kp, &executor, &mut op);
            ops.push(op);
        }

        let result = executor.execute_block(&mut store, &ops);

        // All should succeed.
        for (i, receipt) in result.receipts.iter().enumerate() {
            prop_assert!(receipt.success, "op {i} failed: {:?}", receipt.error);
            prop_assert_eq!(receipt.nonce, i as u64);
        }

        // Final nonce should equal num_ops.
        let state = StateManager::new(&mut store);
        let account = state.get_account(&alice).unwrap().unwrap();
        prop_assert_eq!(account.nonce, num_ops as u64);
    }

    /// State root is deterministic: same operations on same initial state
    /// produce the same root regardless of external factors.
    #[test]
    fn state_root_determinism(
        amount in 1u128..1000,
    ) {
        let executor = BlockExecutor::with_fee_config(FeeConfig {
            base_fee_per_gas: 0,
            ..Default::default()
        });

        // Run 1
        let (mut store1, kp1, alice1, bob1) = setup_with_balances(10_000, 500);
        let mut op1 = UserOperation {
            sender: alice1,
            nonce: 0,
            actions: vec![Action::Transfer { to: bob1, amount }],
            max_fee: 1000,
            signature: vec![],
        };
        sign(&kp1, &executor, &mut op1);
        let result1 = executor.execute_block(&mut store1, &[op1]);

        // Run 2 (same setup, same keypair seed isn't possible with generate(),
        // so we use the same kp)
        let mut store2 = MemoryStore::new();
        apply_genesis(
            &mut store2,
            vec![
                GenesisAccount {
                    id: alice1,
                    balance: 10_000,
                    auth_methods: vec![AuthMethod::Ed25519 {
                        public_key: kp1.public_key(),
                    }],
                },
                GenesisAccount {
                    id: bob1,
                    balance: 500,
                    auth_methods: vec![],
                },
                GenesisAccount {
                    id: treasury_id(),
                    balance: 0,
                    auth_methods: vec![],
                },
            ],
        )
        .unwrap();

        let mut op2 = UserOperation {
            sender: alice1,
            nonce: 0,
            actions: vec![Action::Transfer { to: bob1, amount }],
            max_fee: 1000,
            signature: vec![],
        };
        sign(&kp1, &executor, &mut op2);
        let result2 = executor.execute_block(&mut store2, &[op2]);

        prop_assert_eq!(result1.state_root, result2.state_root);
    }

    /// Balance never goes negative — transfers exceeding balance fail.
    #[test]
    fn no_negative_balance(
        balance in 1u128..10_000,
        transfer in 1u128..20_000,
    ) {
        let (mut store, kp, alice, bob) = setup_with_balances(balance, 0);
        let executor = BlockExecutor::with_fee_config(FeeConfig {
            base_fee_per_gas: 0,
            ..Default::default()
        });

        let mut op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer { to: bob, amount: transfer }],
            max_fee: 1000,
            signature: vec![],
        };
        sign(&kp, &executor, &mut op);

        let result = executor.execute_block(&mut store, &[op]);
        let state = StateManager::new(&mut store);

        let alice_bal = state.get_balance(&alice).unwrap();
        let bob_bal = state.get_balance(&bob).unwrap();

        prop_assert!(alice_bal <= balance, "alice balance increased without reason");
        prop_assert!(bob_bal >= 0, "negative balance detected");

        if transfer > balance {
            prop_assert!(!result.receipts[0].success, "should have failed");
            prop_assert_eq!(alice_bal, balance, "balance should be unchanged on failure");
        }
    }
}
