//! Adversarial consensus tests.
//!
//! Tests for Byzantine behavior: invalid state roots, validator
//! deregistration, double-sign detection, mempool abuse.

use std::sync::Arc;

use solen_consensus::engine::{ConsensusEngine, EngineConfig};
use solen_consensus::mempool::Mempool;
use solen_consensus::slashing::{SlashingEvidence, SlashingReason};
use solen_consensus::validator::{ValidatorInfo, ValidatorSet, ValidatorStatus};
use solen_crypto::Keypair;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_storage::MemoryStore;
use solen_types::account::AuthMethod;

fn setup_engine() -> (ConsensusEngine, Keypair, [u8; 32], [u8; 32]) {
    let kp = Keypair::from_seed(&[0x01; 32]);
    let validator_id = kp.public_key();

    let alice_kp = Keypair::from_seed(&[0x0A; 32]);
    let alice = alice_kp.public_key();

    let mut store = MemoryStore::new();
    apply_genesis(
        &mut store,
        vec![
            GenesisAccount {
                id: validator_id,
                balance: 1_000_000_000_000,
                auth_methods: vec![AuthMethod::Ed25519 {
                    public_key: validator_id,
                }],
            },
            GenesisAccount {
                id: alice,
                balance: 1_000_000_000,
                auth_methods: vec![AuthMethod::Ed25519 {
                    public_key: alice,
                }],
            },
        ],
    )
    .unwrap();

    let config = EngineConfig {
        block_time_ms: 1000,
        max_ops_per_block: 100,
        validator_id,
        chain_id: 1337,
        prune: false,
    };

    let mempool = Mempool::new(1000);
    let engine = ConsensusEngine::new(config, Box::new(store), mempool);

    (engine, alice_kp, alice, validator_id)
}

// ── Test #19: Validator set syncs from staking at epoch ───────

#[test]
fn validator_set_syncs_from_staking_at_epoch() {
    let (engine, _, _, validator_id) = setup_engine();

    // The single-validator engine starts with validator_id in the set.
    // Register a second validator in staking so we can test add/remove.
    let new_validator = [0x99; 32];
    {
        let store = engine.store();
        let mut store = store.write().unwrap();
        let mut staking =
            solen_system_contracts::staking::StakingContract::load(store.as_ref());
        let _ = staking.register_validator(new_validator, 50_000_000_000_000);
        staking.save(store.as_mut());
    }

    // Produce blocks past epoch boundary to trigger transition.
    for _ in 0..101 {
        engine.produce_block();
    }

    // Verify new validator was added to consensus set.
    {
        let vs = engine.validator_set();
        let vs = vs.read().unwrap();
        assert!(
            vs.all().iter().any(|v| v.id == new_validator),
            "new validator should be added at epoch transition"
        );
    }

    // Now deactivate the new validator.
    {
        let store = engine.store();
        let mut store = store.write().unwrap();
        let mut staking =
            solen_system_contracts::staking::StakingContract::load(store.as_ref());
        if let Some(v) = staking.validators.iter_mut().find(|v| v.id == new_validator) {
            v.is_active = false;
        }
        staking.save(store.as_mut());
    }

    // Produce blocks to trigger next epoch.
    for _ in 0..100 {
        engine.produce_block();
    }

    // Verify deactivated validator was removed.
    {
        let vs = engine.validator_set();
        let vs = vs.read().unwrap();
        assert!(
            !vs.all().iter().any(|v| v.id == new_validator),
            "deactivated validator must be removed at epoch transition"
        );
    }
}

// ── Test #20: Double-sign detection ───────────────────────────

#[test]
fn double_sign_detected() {
    use solen_consensus::slashing::check_double_sign;
    use solen_types::block::BlockHeader;

    let proposer = [0x01; 32];

    let header_a = BlockHeader {
        height: 100,
        epoch: 1,
        parent_hash: [0; 32],
        state_root: [0xAA; 32],
        transactions_root: [0; 32],
        receipts_root: [0; 32],
        proposer,
        timestamp_ms: 1000,
    };

    let header_b = BlockHeader {
        height: 100,
        epoch: 1,
        parent_hash: [0; 32],
        state_root: [0xBB; 32], // Different state root = different block
        transactions_root: [0; 32],
        receipts_root: [0; 32],
        proposer,
        timestamp_ms: 1001,
    };

    let evidence = check_double_sign(&header_a, &header_b);
    assert!(
        evidence.is_some(),
        "double-sign at same height must be detected"
    );

    let ev = evidence.unwrap();
    assert_eq!(ev.offender, proposer);
    assert!(matches!(ev.reason, SlashingReason::DoubleSign { .. }));

    // Same block (same state root) should NOT be detected as double-sign.
    let evidence2 = check_double_sign(&header_a, &header_a);
    assert!(
        evidence2.is_none(),
        "same block should not trigger double-sign"
    );
}

// ── Test #14: Intent [0xFF] rejected by mempool ───────────────

#[test]
fn intent_0xff_rejected_by_mempool() {
    let mempool = Mempool::new(100);

    let op = solen_types::transaction::UserOperation {
        sender: [0x01; 32],
        nonce: 0,
        actions: vec![solen_types::transaction::Action::Call {
            target: solen_types::system::INTENT_ADDRESS,
            method: "fulfill".to_string(),
            args: vec![0; 60],
        }],
        max_fee: 100_000,
        signature: vec![0xFF],
    };

    assert!(
        !mempool.submit(op),
        "mempool must reject operations with [0xFF] signature"
    );
}

// ── Test: Slashing penalty calculation ────────────────────────

#[test]
fn slashing_penalty_correct() {
    let double_sign = SlashingReason::DoubleSign {
        height: 1,
        block_a: [0; 32],
        block_b: [1; 32],
    };
    assert_eq!(double_sign.penalty_bps(), 1000); // 10%

    let downtime = SlashingReason::Downtime { missed_blocks: 50 };
    assert_eq!(downtime.penalty_bps(), 100); // 1%

    let invalid_root = SlashingReason::InvalidStateRoot {
        height: 1,
        expected: [0; 32],
        got: [1; 32],
    };
    assert_eq!(invalid_root.penalty_bps(), 500); // 5%
}

// ── Test: Mempool per-sender limit ────────────────────────────

#[test]
fn mempool_per_sender_limit() {
    let mempool = Mempool::new(10_000);
    let sender = [0x01; 32];

    // Submit 16 operations (the limit).
    for i in 0..16u64 {
        let op = solen_types::transaction::UserOperation {
            sender,
            nonce: i,
            actions: vec![],
            max_fee: 100,
            signature: vec![0; 64],
        };
        assert!(mempool.submit(op), "op {} should be accepted", i);
    }

    // 17th should be rejected.
    let op17 = solen_types::transaction::UserOperation {
        sender,
        nonce: 16,
        actions: vec![],
        max_fee: 100,
        signature: vec![0; 64],
    };
    assert!(
        !mempool.submit(op17),
        "17th op from same sender must be rejected"
    );

    // Different sender should still work.
    let op_other = solen_types::transaction::UserOperation {
        sender: [0x02; 32],
        nonce: 0,
        actions: vec![],
        max_fee: 100,
        signature: vec![0; 64],
    };
    assert!(
        mempool.submit(op_other),
        "different sender should be accepted"
    );
}

// ── Test: Mempool dedup by (sender, nonce) ────────────────────

#[test]
fn mempool_rejects_duplicate_nonce() {
    let mempool = Mempool::new(100);

    let op1 = solen_types::transaction::UserOperation {
        sender: [0x01; 32],
        nonce: 5,
        actions: vec![],
        max_fee: 100,
        signature: vec![0; 64],
    };
    assert!(mempool.submit(op1));

    let op2 = solen_types::transaction::UserOperation {
        sender: [0x01; 32],
        nonce: 5, // same nonce
        actions: vec![],
        max_fee: 200, // different fee
        signature: vec![0; 64],
    };
    assert!(!mempool.submit(op2), "duplicate (sender, nonce) must be rejected");
}
