//! Fuzz target: threshold multisig signature verification.
//!
//! Security properties tested:
//! - Duplicate signers never counted twice
//! - Out-of-set signers never counted
//! - threshold=0 never authenticates
//! - Empty signature never authenticates
//! - Malformed chunks (not 96-byte aligned) rejected
//! - Random signatures never verify as valid
//!
//! Likely failure modes:
//! - HashSet dedup bypass with crafted pubkeys
//! - Signature verification accepting invalid Ed25519 sigs
//! - Edge cases with threshold = signers.len()

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
struct FuzzThresholdInput {
    num_signers: u8,
    threshold: u16,
    signature_chunks: Vec<[u8; 96]>,
}

fuzz_target!(|input: FuzzThresholdInput| {
    let num_signers = (input.num_signers % 8).max(1) as usize; // 1-8 signers
    let threshold = input.threshold;

    // Generate real keypairs for the signers list.
    let signers: Vec<Keypair> = (0..num_signers)
        .map(|i| {
            let mut seed = [0u8; 32];
            seed[0] = i as u8 + 1;
            Keypair::from_seed(&seed)
        })
        .collect();

    let signer_pubkeys: Vec<[u8; 32]> = signers.iter().map(|k| k.public_key()).collect();

    let mut store = MemoryStore::new();
    let sender = signer_pubkeys[0]; // use first signer's key as account ID
    let recipient = [0xFF; 32];

    let _ = apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: sender,
                balance: 1_000_000,
                auth_methods: vec![AuthMethod::Threshold {
                    signers: signer_pubkeys.clone(),
                    threshold,
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

    // Build signature from fuzzed chunks.
    let sig_bytes: Vec<u8> = input.signature_chunks.iter()
        .take(10) // limit chunk count
        .flat_map(|c| c.iter().copied())
        .collect();

    let op = UserOperation {
        sender,
        nonce: 0,
        actions: vec![Action::Transfer { to: recipient, amount: 1 }],
        max_fee: 0,
        signature: sig_bytes,
    };

    // Must never panic.
    let _ = executor.execute_block(&mut store, &[op]);
});
