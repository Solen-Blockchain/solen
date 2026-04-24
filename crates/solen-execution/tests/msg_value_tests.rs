//! Tests for the `msg_value()` host function and action-loop accumulator.
//!
//! Each `Action::Call` consumes the sum of `Action::Transfer { to: <target> }`
//! amounts since the last Call to that target (or op start). The primitive is
//! what lets contracts verify deposit amounts without a Transfer-vs-Call
//! mismatch exploit.

use solen_crypto::Keypair;
use solen_execution::executor::BlockExecutor;
use solen_execution::fees::FeeConfig;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_storage::MemoryStore;
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

/// WAT for a contract that reads `msg_value()` and emits it as an event with
/// topic "mv". Data is the 16-byte u128 LE amount.
const PROBE_CONTRACT_WAT: &str = r#"
(module
    (import "env" "msg_value" (func $msg_value (param i32)))
    (import "env" "emit_event" (func $emit (param i32 i32 i32 i32)))
    (memory (export "memory") 1)
    (data (i32.const 16) "mv")
    (func (export "call") (param i32 i32) (result i32)
        ;; Write msg_value's 16 bytes at mem[0..16].
        (call $msg_value (i32.const 0))
        ;; Emit event: topic="mv" at offset 16 (len 2), data at offset 0 (len 16).
        (call $emit (i32.const 16) (i32.const 2) (i32.const 0) (i32.const 16))
        (i32.const 0)
    )
)
"#;

struct Fixture {
    store: MemoryStore,
    kp: Keypair,
    sender: AccountId,
    executor: BlockExecutor,
    contract: AccountId,
}

fn setup() -> Fixture {
    let mut store = MemoryStore::new();
    let kp = Keypair::from_seed(&[0x42; 32]);
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

    let executor = zero_fee_executor();
    let wasm = wat::parse_str(PROBE_CONTRACT_WAT).unwrap();

    let mut deploy_op = UserOperation {
        sender,
        nonce: 0,
        actions: vec![Action::Deploy {
            code: wasm,
            salt: [0u8; 32],
        }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&kp, &executor, &mut deploy_op);

    let result = executor.execute_block(&mut store, &[deploy_op]);
    assert!(result.receipts[0].success, "deploy failed: {:?}", result.receipts[0].error);

    // Extract deployed contract address from the "deploy" event.
    let deploy_event = result.receipts[0]
        .events
        .iter()
        .find(|e| e.topic == b"deploy")
        .expect("deploy event");
    let mut contract = [0u8; 32];
    contract.copy_from_slice(&deploy_event.data[..32]);

    Fixture {
        store,
        kp,
        sender,
        executor,
        contract,
    }
}

/// Extract all mv-topic events from the receipt in order (one per Call to the contract).
fn extract_msg_values(events: &[solen_execution::receipt::Event]) -> Vec<u128> {
    events
        .iter()
        .filter(|e| e.topic == b"mv")
        .map(|e| {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&e.data[..16]);
            u128::from_le_bytes(buf)
        })
        .collect()
}

// ── A: Transfer + Call ────────────────────────────────────────
#[test]
fn msg_value_single_transfer_then_call() {
    let mut fx = setup();

    let mut op = UserOperation {
        sender: fx.sender,
        nonce: 1,
        actions: vec![
            Action::Transfer {
                to: fx.contract,
                amount: 12_345,
            },
            Action::Call {
                target: fx.contract,
                method: "probe".to_string(),
                args: vec![],
            },
        ],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&fx.kp, &fx.executor, &mut op);

    let result = fx.executor.execute_block(&mut fx.store, &[op]);
    assert!(result.receipts[0].success, "{:?}", result.receipts[0].error);

    let mvs = extract_msg_values(&result.receipts[0].events);
    assert_eq!(mvs, vec![12_345u128], "msg_value should equal the Transfer amount");
}

// ── B: Multiple Transfers then one Call ──────────────────────
#[test]
fn msg_value_multiple_transfers_sum() {
    let mut fx = setup();

    let mut op = UserOperation {
        sender: fx.sender,
        nonce: 1,
        actions: vec![
            Action::Transfer { to: fx.contract, amount: 100 },
            Action::Transfer { to: fx.contract, amount: 250 },
            Action::Transfer { to: fx.contract, amount: 50 },
            Action::Call {
                target: fx.contract,
                method: "probe".to_string(),
                args: vec![],
            },
        ],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&fx.kp, &fx.executor, &mut op);

    let result = fx.executor.execute_block(&mut fx.store, &[op]);
    assert!(result.receipts[0].success, "{:?}", result.receipts[0].error);

    let mvs = extract_msg_values(&result.receipts[0].events);
    assert_eq!(mvs, vec![400u128], "msg_value should equal the sum of preceding Transfers");
}

// ── C: Call with no preceding Transfer ───────────────────────
#[test]
fn msg_value_is_zero_without_transfer() {
    let mut fx = setup();

    let mut op = UserOperation {
        sender: fx.sender,
        nonce: 1,
        actions: vec![Action::Call {
            target: fx.contract,
            method: "probe".to_string(),
            args: vec![],
        }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&fx.kp, &fx.executor, &mut op);

    let result = fx.executor.execute_block(&mut fx.store, &[op]);
    assert!(result.receipts[0].success, "{:?}", result.receipts[0].error);

    let mvs = extract_msg_values(&result.receipts[0].events);
    assert_eq!(mvs, vec![0u128]);
}

// ── D: Two Calls with Transfers interleaved — each call gets its own window ──
#[test]
fn msg_value_resets_between_calls() {
    let mut fx = setup();

    let mut op = UserOperation {
        sender: fx.sender,
        nonce: 1,
        actions: vec![
            Action::Transfer { to: fx.contract, amount: 10 },
            Action::Call {
                target: fx.contract,
                method: "probe".to_string(),
                args: vec![],
            },
            Action::Transfer { to: fx.contract, amount: 20 },
            Action::Transfer { to: fx.contract, amount: 5 },
            Action::Call {
                target: fx.contract,
                method: "probe".to_string(),
                args: vec![],
            },
            Action::Call {
                target: fx.contract,
                method: "probe".to_string(),
                args: vec![],
            },
        ],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&fx.kp, &fx.executor, &mut op);

    let result = fx.executor.execute_block(&mut fx.store, &[op]);
    assert!(result.receipts[0].success, "{:?}", result.receipts[0].error);

    let mvs = extract_msg_values(&result.receipts[0].events);
    assert_eq!(mvs, vec![10u128, 25u128, 0u128]);
}

// ── E: Exploit shape — Transfer(1) + Call with claimed=BIG ────
// This is the solenswap-style attack. The VM should report msg_value=1,
// giving the contract the primitive it needs to reject the claim.
#[test]
fn msg_value_exposes_mismatch_for_exploit_rejection() {
    let mut fx = setup();

    let mut op = UserOperation {
        sender: fx.sender,
        nonce: 1,
        actions: vec![
            Action::Transfer {
                to: fx.contract,
                amount: 1,
            },
            Action::Call {
                target: fx.contract,
                method: "buy".to_string(),
                // Args would carry a claimed "amount=999_999". The contract
                // compares `claimed` against msg_value() and rejects if >.
                args: vec![0xFF; 16],
            },
        ],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(&fx.kp, &fx.executor, &mut op);

    let result = fx.executor.execute_block(&mut fx.store, &[op]);
    assert!(result.receipts[0].success, "{:?}", result.receipts[0].error);

    let mvs = extract_msg_values(&result.receipts[0].events);
    assert_eq!(
        mvs,
        vec![1u128],
        "msg_value must report the ACTUAL transferred amount (1), not the claimed one"
    );
}
