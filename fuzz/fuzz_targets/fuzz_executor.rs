//! Fuzz target: feed random user operations into the block executor.
//!
//! Ensures no panics, memory corruption, or undefined behavior regardless
//! of operation content.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use solen_execution::executor::BlockExecutor;
use solen_execution::fees::FeeConfig;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_storage::MemoryStore;
use solen_types::account::AuthMethod;
use solen_types::transaction::{Action, UserOperation};

#[derive(Debug, Arbitrary)]
struct FuzzInput {
    sender_balance: u64,
    nonce: u64,
    actions: Vec<FuzzAction>,
    signature: Vec<u8>,
}

#[derive(Debug, Arbitrary)]
enum FuzzAction {
    Transfer { to_byte: u8, amount: u64 },
    Call { target_byte: u8, method_len: u8 },
    Deploy { code_len: u8, salt_byte: u8 },
}

fuzz_target!(|input: FuzzInput| {
    let mut store = MemoryStore::new();

    let sender = {
        let mut id = [0u8; 32];
        id[0] = 1;
        id
    };
    let receiver = {
        let mut id = [0u8; 32];
        id[0] = 2;
        id
    };

    let _ = apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: sender,
                balance: input.sender_balance as u128,
                auth_methods: vec![],
            },
            GenesisAccount {
                id: receiver,
                balance: 0,
                auth_methods: vec![],
            },
        ],
    );

    let actions: Vec<Action> = input
        .actions
        .iter()
        .take(5) // limit to avoid OOM
        .map(|a| match a {
            FuzzAction::Transfer { to_byte, amount } => {
                let mut to = [0u8; 32];
                to[0] = *to_byte;
                Action::Transfer {
                    to,
                    amount: *amount as u128,
                }
            }
            FuzzAction::Call {
                target_byte,
                method_len,
            } => {
                let mut target = [0u8; 32];
                target[0] = *target_byte;
                Action::Call {
                    target,
                    method: "x".repeat((*method_len).min(32) as usize),
                    args: vec![],
                }
            }
            FuzzAction::Deploy {
                code_len,
                salt_byte,
            } => Action::Deploy {
                code: vec![0; (*code_len).min(64) as usize],
                salt: [*salt_byte; 32],
            },
        })
        .collect();

    let op = UserOperation {
        sender,
        nonce: input.nonce,
        actions,
        max_fee: u128::MAX,
        signature: input.signature,
    };

    let executor = BlockExecutor::with_fee_config(FeeConfig {
        base_fee_per_gas: 0,
        ..Default::default()
    });

    // This must never panic.
    let _ = executor.execute_block(&mut store, &[op]);
});
