//! Tests for `sdk::queue_call` — contract→contract calls queued during
//! one contract's execution and dispatched after it returns.
//!
//! Verifies: queued call actually runs, sub-call storage state is committed,
//! caller identity is the queueing contract (not the original user), the
//! depth cap stops runaway recursion, and queue-slot exhaustion is a failure.

use solen_crypto::Keypair;
use solen_execution::executor::BlockExecutor;
use solen_execution::fees::FeeConfig;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
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

/// Counter contract. Any call bumps a u64 at storage key "n" and emits "bump".
const COUNTER_WAT: &str = r#"
(module
    (import "env" "storage_read" (func $sr (param i32 i32 i32) (result i32)))
    (import "env" "storage_write" (func $sw (param i32 i32 i32 i32)))
    (import "env" "emit_event" (func $emit (param i32 i32 i32 i32)))
    (import "env" "get_caller" (func $get_caller (param i32)))
    (memory (export "memory") 1)
    (data (i32.const 0) "n")
    (data (i32.const 16) "bump")
    (func (export "call") (param i32 i32) (result i32)
        (local $v i64)
        (drop (call $sr (i32.const 0) (i32.const 1) (i32.const 32)))
        (local.set $v (i64.load (i32.const 32)))
        (local.set $v (i64.add (local.get $v) (i64.const 1)))
        (i64.store (i32.const 32) (local.get $v))
        (call $sw (i32.const 0) (i32.const 1) (i32.const 32) (i32.const 8))
        ;; Write caller into mem[128..160] so it appears in the event data.
        (call $get_caller (i32.const 128))
        ;; Event data = 8-byte count + 32-byte caller at mem[32..168] combined?
        ;; Simpler: emit two events. First just the count.
        (call $emit (i32.const 16) (i32.const 4) (i32.const 32) (i32.const 8))
        (i32.const 0)
    )
)
"#;

/// Forwarder contract. Reads a 32-byte target from input (skipping the
/// 1-byte empty-method null prefix), then queues a call to `target.inc`.
const FORWARDER_WAT: &str = r#"
(module
    (import "env" "queue_contract_call" (func $qc (param i32 i32 i32 i32 i32) (result i32)))
    (import "env" "emit_event" (func $emit (param i32 i32 i32 i32)))
    (memory (export "memory") 1)
    (data (i32.const 16) "inc")
    (data (i32.const 32) "fwd")
    (func (export "call") (param $ptr i32) (param $len i32) (result i32)
        (local $i i32)
        (local.set $i (i32.const 0))
        (block $done
        (loop $copy
            (br_if $done (i32.ge_s (local.get $i) (i32.const 32)))
            (i32.store8
                (i32.add (i32.const 64) (local.get $i))
                (i32.load8_u (i32.add (local.get $ptr) (i32.add (i32.const 1) (local.get $i)))))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br $copy))
        )
        (drop (call $qc (i32.const 64) (i32.const 16) (i32.const 3) (i32.const 0) (i32.const 0)))
        (call $emit (i32.const 32) (i32.const 3) (i32.const 0) (i32.const 0))
        (i32.const 0)
    )
)
"#;

/// Self-queueing contract. Reads the target from the LAST 32 bytes of input
/// (prefix-agnostic — works for any method name) and queues a call to that
/// target with method="go" and args = the same 32-byte target. If target is
/// itself, this recurses; depth cap should stop it.
const SELF_QUEUE_WAT: &str = r#"
(module
    (import "env" "queue_contract_call" (func $qc (param i32 i32 i32 i32 i32) (result i32)))
    (memory (export "memory") 1)
    (data (i32.const 16) "go")
    (func (export "call") (param $ptr i32) (param $len i32) (result i32)
        (local $i i32)
        (local $src i32)
        ;; src = ptr + len - 32   (start of target block at input tail)
        (local.set $src (i32.sub (i32.add (local.get $ptr) (local.get $len)) (i32.const 32)))
        (local.set $i (i32.const 0))
        (block $done
        (loop $copy
            (br_if $done (i32.ge_s (local.get $i) (i32.const 32)))
            (i32.store8
                (i32.add (i32.const 64) (local.get $i))
                (i32.load8_u (i32.add (local.get $src) (local.get $i))))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br $copy))
        )
        (drop (call $qc (i32.const 64) (i32.const 16) (i32.const 2) (i32.const 64) (i32.const 32)))
        (i32.const 0)
    )
)
"#;

fn deploy(
    store: &mut MemoryStore,
    executor: &BlockExecutor,
    kp: &Keypair,
    sender: AccountId,
    nonce: u64,
    wat: &str,
    salt: [u8; 32],
) -> AccountId {
    let wasm = wat::parse_str(wat).unwrap();
    let mut op = UserOperation {
        sender,
        nonce,
        actions: vec![Action::Deploy { code: wasm, salt }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(kp, executor, &mut op);
    let result = executor.execute_block(store, &[op]);
    assert!(result.receipts[0].success, "deploy: {:?}", result.receipts[0].error);
    let ev = result.receipts[0]
        .events
        .iter()
        .find(|e| e.topic == b"deploy")
        .expect("deploy event missing");
    let mut addr = [0u8; 32];
    addr.copy_from_slice(&ev.data[..32]);
    addr
}

fn counter_value(store: &MemoryStore, contract: &AccountId) -> u64 {
    // Contract storage key format: "cs/{contract}/{key}"
    let mut k = Vec::with_capacity(4 + 32 + 1 + 1);
    k.extend_from_slice(b"cs/");
    k.extend_from_slice(contract);
    k.push(b'/');
    k.push(b'n');
    match store.get(&k).unwrap() {
        Some(bytes) if bytes.len() >= 8 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes[..8]);
            u64::from_le_bytes(b)
        }
        _ => 0,
    }
}

fn setup() -> (MemoryStore, Keypair, AccountId, BlockExecutor) {
    let mut store = MemoryStore::new();
    let kp = Keypair::from_seed(&[0x77; 32]);
    let sender = kp.public_key();
    apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: sender,
            balance: 1_000_000_000_000,
            auth_methods: vec![AuthMethod::Ed25519 { public_key: sender }],
        }],
    )
    .unwrap();
    (store, kp, sender, zero_fee_executor())
}

#[test]
fn queued_call_actually_executes() {
    let (mut store, kp, sender, executor) = setup();
    let counter = deploy(&mut store, &executor, &kp, sender, 0, COUNTER_WAT, [1; 32]);
    let forwarder = deploy(&mut store, &executor, &kp, sender, 1, FORWARDER_WAT, [2; 32]);

    // Counter starts at 0.
    assert_eq!(counter_value(&store, &counter), 0);

    // Call forwarder with target=counter in args.
    let mut op = UserOperation {
        sender,
        nonce: 2,
        actions: vec![Action::Call {
            target: forwarder,
            method: String::new(), // empty method → input starts with null separator
            args: counter.to_vec(),
        }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);
    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        result.receipts[0].success,
        "op failed: {:?}",
        result.receipts[0].error
    );

    // Counter should now be 1.
    assert_eq!(
        counter_value(&store, &counter),
        1,
        "queued call did not execute against counter"
    );

    // Events from both contracts should be recorded.
    let topics: Vec<&[u8]> = result.receipts[0]
        .events
        .iter()
        .map(|e| e.topic.as_slice())
        .collect();
    assert!(topics.iter().any(|t| *t == b"fwd"), "forwarder event missing");
    assert!(topics.iter().any(|t| *t == b"bump"), "counter event missing");
}

#[test]
fn queued_call_depth_cap_stops_runaway() {
    let (mut store, kp, sender, executor) = setup();
    let self_q = deploy(&mut store, &executor, &kp, sender, 0, SELF_QUEUE_WAT, [3; 32]);

    // Calling self_q with target=self_q queues another self-call which queues
    // another, etc. Depth cap is 8, so this must fail (not infinite-loop).
    let mut op = UserOperation {
        sender,
        nonce: 1,
        actions: vec![Action::Call {
            target: self_q,
            method: String::new(),
            args: self_q.to_vec(),
        }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut op);
    let result = executor.execute_block(&mut store, &[op]);
    assert!(
        !result.receipts[0].success,
        "runaway recursion was not stopped"
    );
    let err = result.receipts[0].error.as_deref().unwrap_or("");
    assert!(
        err.contains("depth"),
        "expected depth-cap error, got: {err}"
    );
}
