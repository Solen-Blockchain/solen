//! TPS benchmark: measures actual throughput of the block executor.
//!
//! Run with: cargo bench -p solen-execution --bench tps

use std::time::Instant;

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

fn setup_store(num_accounts: u32, balance: u128) -> (MemoryStore, Vec<(Keypair, AccountId)>) {
    let mut store = MemoryStore::new();
    let mut accounts = Vec::new();

    for i in 0..num_accounts {
        let kp = Keypair::from_seed(&{
            let mut s = [0u8; 32];
            s[..4].copy_from_slice(&i.to_le_bytes());
            s[4] = 0xFF; // avoid collision with account IDs
            s
        });
        let id = make_id(i);
        accounts.push((kp, id));
    }

    let genesis: Vec<GenesisAccount> = accounts
        .iter()
        .map(|(kp, id)| GenesisAccount {
            id: *id,
            balance,
            auth_methods: vec![AuthMethod::Ed25519 {
                public_key: kp.public_key(),
            }],
        })
        .collect();

    apply_genesis(&mut store, genesis).unwrap();
    (store, accounts)
}

fn sign_op(kp: &Keypair, executor: &BlockExecutor, op: &mut UserOperation) {
    let msg = executor.operation_signing_message(op);
    op.signature = kp.sign(&msg).to_vec();
}

fn bench_transfers(num_ops: usize) {
    let num_accounts = (num_ops as u32).max(100);
    let (mut store, accounts) = setup_store(num_accounts, 1_000_000_000);

    let executor = BlockExecutor::with_fee_config(FeeConfig {
        base_fee_per_gas: 0,
        ..Default::default()
    });

    // Pre-build all operations.
    let mut ops: Vec<UserOperation> = Vec::with_capacity(num_ops);
    for i in 0..num_ops {
        let sender_idx = i as u32;
        let receiver_idx = ((i + 1) % num_accounts as usize) as u32;
        let (kp, sender_id) = &accounts[sender_idx as usize];

        let mut op = UserOperation {
            sender: *sender_id,
            nonce: 0,
            actions: vec![Action::Transfer {
                to: make_id(receiver_idx),
                amount: 1,
            }],
            max_fee: 100_000,
            signature: vec![],
        };
        sign_op(kp, &executor, &mut op);
        ops.push(op);
    }

    // Execute and time.
    let start = Instant::now();
    let result = executor.execute_block(&mut store, &ops);
    let elapsed = start.elapsed();

    let successful = result.receipts.iter().filter(|r| r.success).count();
    let tps = num_ops as f64 / elapsed.as_secs_f64();

    println!("Transfer benchmark ({num_ops} ops):");
    println!("  Successful: {successful}/{num_ops}");
    println!("  Time:       {:.3}ms", elapsed.as_secs_f64() * 1000.0);
    println!("  TPS:        {tps:.0}");
    println!("  Gas:        {}", result.gas_used);
    println!();
}

fn bench_contract_calls(num_ops: usize) {
    let (mut store, accounts) = setup_store(100, 1_000_000_000);

    let executor = BlockExecutor::with_fee_config(FeeConfig {
        base_fee_per_gas: 0,
        ..Default::default()
    });

    // Deploy a counter contract first.
    let wasm = wat::parse_str(COUNTER_WAT).expect("WAT parse failed");
    let (deploy_kp, deploy_id) = &accounts[0];
    let mut deploy_op = UserOperation {
        sender: *deploy_id,
        nonce: 0,
        actions: vec![Action::Deploy {
            code: wasm.to_vec(),
            salt: [42u8; 32],
        }],
        max_fee: 1_000_000,
        signature: vec![],
    };
    sign_op(deploy_kp, &executor, &mut deploy_op);
    let deploy_result = executor.execute_block(&mut store, &[deploy_op]);
    assert!(deploy_result.receipts[0].success);

    let contract_id = {
        let mut id = [0u8; 32];
        id.copy_from_slice(&deploy_result.receipts[0].events[0].data);
        id
    };

    // Build call operations (different senders to avoid nonce issues).
    let mut ops: Vec<UserOperation> = Vec::with_capacity(num_ops);
    for i in 0..num_ops {
        let sender_idx = (i % 99) + 1; // skip account 0 (nonce already 1)
        let (kp, sender_id) = &accounts[sender_idx];

        let mut op = UserOperation {
            sender: *sender_id,
            nonce: (i / 99) as u64,
            actions: vec![Action::Call {
                target: contract_id,
                method: "increment".to_string(),
                args: vec![],
            }],
            max_fee: 1_000_000,
            signature: vec![],
        };
        sign_op(kp, &executor, &mut op);
        ops.push(op);
    }

    let start = Instant::now();
    let result = executor.execute_block(&mut store, &ops);
    let elapsed = start.elapsed();

    let successful = result.receipts.iter().filter(|r| r.success).count();
    let tps = num_ops as f64 / elapsed.as_secs_f64();

    println!("Contract call benchmark ({num_ops} ops):");
    println!("  Successful: {successful}/{num_ops}");
    println!("  Time:       {:.3}ms", elapsed.as_secs_f64() * 1000.0);
    println!("  TPS:        {tps:.0}");
    println!("  Gas:        {}", result.gas_used);
    println!();
}

const COUNTER_WAT: &str = r#"
(module
    (import "env" "storage_read" (func $storage_read (param i32 i32 i32) (result i32)))
    (import "env" "storage_write" (func $storage_write (param i32 i32 i32 i32)))
    (import "env" "emit_event" (func $emit_event (param i32 i32 i32 i32)))
    (import "env" "get_caller" (func $get_caller (param i32)))
    (import "env" "get_block_height" (func $get_block_height (result i64)))
    (import "env" "set_return_data" (func $set_return_data (param i32 i32)))
    (memory (export "memory") 1)
    (data (i32.const 0) "count")
    (data (i32.const 100) "incremented")
    (func (export "call") (param $input_ptr i32) (param $input_len i32) (result i32)
        (local $val i32)
        (drop (call $storage_read (i32.const 0) (i32.const 5) (i32.const 200)))
        (local.set $val (i32.load (i32.const 200)))
        (local.set $val (i32.add (local.get $val) (i32.const 1)))
        (i32.store (i32.const 200) (local.get $val))
        (call $storage_write (i32.const 0) (i32.const 5) (i32.const 200) (i32.const 4))
        (call $emit_event (i32.const 100) (i32.const 11) (i32.const 200) (i32.const 4))
        (call $set_return_data (i32.const 200) (i32.const 4))
        (i32.const 4)
    )
)
"#;

/// Transfer benchmark with no auth (isolates state-only cost).
fn bench_transfers_no_auth(num_ops: usize) {
    let num_accounts = (num_ops as u32).max(100);

    let mut store = MemoryStore::new();
    let mut accounts = Vec::new();
    for i in 0..num_accounts {
        let id = make_id(i);
        apply_genesis(
            &mut store,
            vec![GenesisAccount {
                id,
                balance: 1_000_000_000,
                auth_methods: vec![], // no auth = skip sig verification
            }],
        )
        .unwrap();
        accounts.push(id);
    }

    let executor = BlockExecutor::with_fee_config(FeeConfig {
        base_fee_per_gas: 0,
        ..Default::default()
    });

    let mut ops: Vec<UserOperation> = Vec::with_capacity(num_ops);
    for i in 0..num_ops {
        let sender_idx = i as u32;
        let receiver_idx = ((i + 1) % num_accounts as usize) as u32;
        ops.push(UserOperation {
            sender: accounts[sender_idx as usize],
            nonce: 0,
            actions: vec![Action::Transfer {
                to: make_id(receiver_idx),
                amount: 1,
            }],
            max_fee: 100_000,
            signature: vec![],
        });
    }

    let start = Instant::now();
    let result = executor.execute_block(&mut store, &ops);
    let elapsed = start.elapsed();

    let successful = result.receipts.iter().filter(|r| r.success).count();
    let tps = num_ops as f64 / elapsed.as_secs_f64();

    println!("Transfer NO-AUTH benchmark ({num_ops} ops):");
    println!("  Successful: {successful}/{num_ops}");
    println!("  Time:       {:.3}ms", elapsed.as_secs_f64() * 1000.0);
    println!("  TPS:        {tps:.0}");
    println!();
}

fn main() {
    println!("=== Solen TPS Benchmark ===\n");

    bench_transfers(100);
    bench_transfers(1_000);
    bench_transfers(10_000);

    println!("--- Without signature verification ---\n");
    bench_transfers_no_auth(10_000);

    println!("--- Contract calls ---\n");
    bench_contract_calls(100);
    bench_contract_calls(1_000);
}
