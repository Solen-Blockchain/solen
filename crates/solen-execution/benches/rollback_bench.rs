//! Benchmark: multi-action rollback cost on large stores.
//!
//! Measures how long it takes to roll back a failing multi-action operation
//! when the store has N entries. This is the worst-case DoS cost.
//!
//! Run with: cargo bench -p solen-execution --bench rollback_bench

use std::time::Instant;

use solen_crypto::Keypair;
use solen_execution::executor::BlockExecutor;
use solen_execution::fees::FeeConfig;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_storage::{MemoryStore, StateStore};
use solen_types::account::AuthMethod;
use solen_types::transaction::{Action, UserOperation};

fn sign_op(kp: &Keypair, executor: &BlockExecutor, op: &mut UserOperation) {
    let msg = executor.operation_signing_message(op);
    op.signature = kp.sign(&msg).to_vec();
}

fn bench_rollback(store_entries: usize) {
    let mut store = MemoryStore::new();
    let kp = Keypair::from_seed(&[0x0A; 32]);
    let alice = kp.public_key();
    let bob_kp = Keypair::from_seed(&[0x0B; 32]);
    let bob = bob_kp.public_key();

    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: alice,
                balance: 1_000_000_000_000,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: alice }],
            },
            GenesisAccount {
                id: bob,
                balance: 1_000_000_000,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: bob }],
            },
        ],
    )
    .unwrap();

    // Populate store with N entries to simulate a large chain state.
    for i in 0..store_entries {
        let key = format!("padding/{:08}", i);
        let val = format!("value_{:08}", i);
        store.put(key.as_bytes(), val.as_bytes()).unwrap();
    }

    let executor = BlockExecutor::with_fee_config(FeeConfig {
        base_fee_per_gas: 0,
        ..Default::default()
    });

    // Multi-action operation: first action succeeds, second always fails.
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
                amount: u128::MAX, // Will fail — insufficient balance.
            },
        ],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);

    // Measure the rollback cost.
    let start = Instant::now();
    let result = executor.execute_block(&mut store, &[op]);
    let elapsed = start.elapsed();

    let success = result.receipts[0].success;
    assert!(!success, "operation should fail");

    println!(
        "Rollback with {:>8} store entries: {:>8.3}ms",
        store_entries,
        elapsed.as_secs_f64() * 1000.0
    );
}

fn main() {
    println!("=== Multi-Action Rollback Benchmark ===\n");
    println!("Measures worst-case rollback time for a failing multi-action");
    println!("operation on stores of increasing size.\n");

    bench_rollback(100);
    bench_rollback(1_000);
    bench_rollback(10_000);
    bench_rollback(100_000);
    bench_rollback(500_000);

    println!("\nNote: On RocksDB, snapshot creation uses hard-linked checkpoints");
    println!("(near-instant), but scan_all for rollback is O(N).");
}
