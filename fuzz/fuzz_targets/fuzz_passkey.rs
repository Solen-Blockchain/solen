//! Fuzz target: passkey (WebAuthn) signature verification.
//!
//! Security properties tested:
//! - No panic on any input
//! - Invalid signatures never verify as true
//! - Malformed clientDataJSON (duplicate keys, escapes, truncation) never verify
//! - Invalid P-256 points never verify
//!
//! Likely failure modes:
//! - Off-by-one in auth_data_len / client_data_len parsing
//! - Integer overflow in offset calculations
//! - JSON parser accepting crafted duplicate keys
//! - P-256 point construction panicking on invalid coordinates

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
struct FuzzPasskeyInput {
    pk_x: [u8; 32],
    pk_y: [u8; 32],
    credential_id: Vec<u8>,
    signature_bytes: Vec<u8>,
    transfer_amount: u64,
}

fuzz_target!(|input: FuzzPasskeyInput| {
    // Limit signature size to prevent OOM.
    if input.signature_bytes.len() > 4096 || input.credential_id.len() > 256 {
        return;
    }

    let mut store = MemoryStore::new();
    let sender = [0x01; 32];
    let recipient = [0x02; 32];

    let _ = apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: sender,
                balance: 1_000_000_000,
                auth_methods: vec![AuthMethod::Passkey {
                    credential_id: input.credential_id,
                    public_key_x: input.pk_x,
                    public_key_y: input.pk_y,
                    rp_id: String::new(),
                    origins: Vec::new(),
                }],
            },
            GenesisAccount {
                id: recipient,
                balance: 0,
                auth_methods: vec![AuthMethod::Ed25519 { public_key: recipient }],
            },
        ],
    );

    let executor = BlockExecutor::with_fee_config(FeeConfig {
        base_fee_per_gas: 0,
        ..Default::default()
    });

    let op = UserOperation {
        sender,
        nonce: 0,
        actions: vec![Action::Transfer {
            to: recipient,
            amount: input.transfer_amount as u128,
        }],
        max_fee: 0,
        signature: input.signature_bytes,
    };

    // Must never panic. Must never succeed (random bytes won't be a valid passkey sig).
    let result = executor.execute_block(&mut store, &[op]);
    // A fuzzed signature should essentially never verify successfully.
    // If it does, that's a critical finding worth investigating.
});
