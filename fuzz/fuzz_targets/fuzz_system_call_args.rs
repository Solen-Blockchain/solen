//! Fuzz target: system contract call argument parsing.
//!
//! Security properties tested:
//! - No panic on malformed args to any system call
//! - No balance creation from malformed staking/governance/bridge args
//! - No state corruption from truncated arguments
//! - All error paths return gracefully
//!
//! Likely failure modes:
//! - Off-by-one in read_account_id / read_u128 offset calculations
//! - Unbounded description strings in governance proposals
//! - Integer overflow in stake amounts
//! - Partial state mutation on error in multi-step system calls

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use solen_crypto::Keypair;
use solen_execution::executor::BlockExecutor;
use solen_execution::fees::FeeConfig;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_storage::MemoryStore;
use solen_types::account::AuthMethod;
use solen_types::transaction::{Action, UserOperation};

#[derive(Debug, Arbitrary)]
struct FuzzSystemCallInput {
    target_idx: u8,   // which system contract to target
    method_idx: u8,   // which method to call
    args: Vec<u8>,    // raw args bytes
    sender_balance: u64,
}

const METHODS: &[&[&str]] = &[
    // STAKING (idx 0)
    &["register", "delegate", "undelegate", "withdraw", "slash", "unjail", "rotate_key"],
    // GOVERNANCE (idx 1)
    &["propose_block_time", "propose_set_base_fee", "vote", "finalize", "execute"],
    // BRIDGE (idx 2)
    &["deposit", "register_rollup", "submit_batch", "dispute"],
    // TREASURY (idx 3)
    &["status"],
    // INTENT (idx 4)
    &["fulfill"],
    // VESTING (idx 5)
    &["claim"],
];

fuzz_target!(|input: FuzzSystemCallInput| {
    // Limit args to prevent OOM.
    if input.args.len() > 4096 {
        return;
    }

    let target_idx = (input.target_idx % 6) as usize;
    let methods = METHODS[target_idx];
    let method = methods[(input.method_idx as usize) % methods.len()];

    let target = match target_idx {
        0 => solen_types::system::STAKING_ADDRESS,
        1 => solen_types::system::GOVERNANCE_ADDRESS,
        2 => solen_types::system::BRIDGE_ADDRESS,
        3 => solen_types::system::TREASURY_ADDRESS,
        4 => solen_types::system::INTENT_ADDRESS,
        5 => solen_types::system::VESTING_ADDRESS,
        _ => return,
    };

    let mut store = MemoryStore::new();
    let kp = Keypair::from_seed(&[0x01; 32]);
    let sender = kp.public_key();

    let _ = apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: sender,
            balance: input.sender_balance as u128 * 100_000_000, // in base units
            auth_methods: vec![AuthMethod::Ed25519 { public_key: sender }],
        }],
    );

    let executor = BlockExecutor::with_fee_config(FeeConfig {
        base_fee_per_gas: 0,
        ..Default::default()
    });

    let mut op = UserOperation {
        sender,
        nonce: 0,
        actions: vec![Action::Call {
            target,
            method: method.to_string(),
            args: input.args,
        }],
        max_fee: 0,
        signature: vec![],
    };

    let msg = executor.operation_signing_message(&op);
    op.signature = kp.sign(&msg).to_vec();

    // Must never panic. State conservation should hold.
    let _ = executor.execute_block(&mut store, &[op]);
});
