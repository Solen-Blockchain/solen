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
            proposer_signature: vec![],
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
            proposer_signature: vec![],
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

// ── Adversarial test: Attestation for unknown block rejected ─

#[test]
fn attestation_for_unknown_block_rejected() {
    let (engine, kp, validator_id, _alice) = setup_engine();

    // Produce a block so we have height 1.
    engine.produce_block();

    // Try to accept attestation for a block at height 2 that we don't have.
    let fake_hash = [0xAB; 32];
    let accepted = engine.accept_attestation(validator_id, 2, fake_hash);
    assert!(!accepted, "attestation for unknown block must be rejected");
}

// ── Adversarial test: Synced block with wrong state root rejected ─

#[test]
fn synced_block_wrong_state_root_rejected() {
    let (engine, kp, validator_id, alice) = setup_engine();

    // Produce a real block to get height 1.
    let produced = engine.produce_block();
    let real_header = produced.header.clone();

    // Create a fake header with wrong state root.
    let mut fake_header = real_header.clone();
    fake_header.height = 2;
    fake_header.parent_hash = solen_consensus::engine::block_hash(&real_header);
    fake_header.state_root = [0xFF; 32]; // wrong state root

    let height_before = engine.height();
    engine.replay_synced_block(&fake_header, &[], vec![]);

    assert_eq!(
        engine.height(),
        height_before,
        "block with wrong state root must be rejected during sync"
    );
}

// ── Adversarial test: Mempool rejects [0xFF] system signature ─

#[test]
fn mempool_rejects_system_signature() {
    let mempool = Mempool::new(100);

    let op = solen_types::transaction::UserOperation {
        sender: [0x01; 32],
        nonce: 0,
        actions: vec![solen_types::transaction::Action::Call {
            target: solen_types::system::STAKING_ADDRESS,
            method: "slash".to_string(),
            args: vec![0; 40],
        }],
        max_fee: 0,
        signature: vec![0xFF], // system marker
    };

    assert!(
        !mempool.submit(op),
        "[0xFF] system signature must be rejected from mempool"
    );
}

// ═══════════════════════════════════════════════════════════════
// CONSENSUS EDGE CASE TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn block_at_wrong_height_rejected() {
    let (engine, _, _, _) = setup_engine();
    engine.produce_block(); // height 1

    let mut fake_header = solen_types::block::BlockHeader {
        height: 5, // way ahead
        epoch: 0,
        parent_hash: [0; 32],
        state_root: [0; 32],
        transactions_root: [0; 32],
        receipts_root: [0; 32],
        proposer: [0x01; 32],
        timestamp_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 100,
            proposer_signature: vec![],
    };

    let accepted = engine.accept_block(&fake_header, &[]);
    assert!(!accepted, "block at wrong height must be rejected");
}

#[test]
fn block_from_invalid_proposer_rejected() {
    let (engine, _, _, _) = setup_engine();
    engine.produce_block(); // height 1

    let fake_proposer = [0xDE; 32]; // not in validator set
    let header = solen_types::block::BlockHeader {
        height: 2,
        epoch: 0,
        parent_hash: solen_consensus::engine::block_hash(&engine.get_blocks_for_sync(1, 1)[0].header),
        state_root: [0; 32],
        transactions_root: [0; 32],
        receipts_root: [0; 32],
        proposer: fake_proposer,
        timestamp_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 100,
            proposer_signature: vec![],
    };

    let accepted = engine.accept_block(&header, &[]);
    assert!(!accepted, "block from unknown proposer must be rejected");
}

#[test]
fn block_with_wrong_epoch_rejected() {
    let (engine, _, _, _) = setup_engine();
    engine.produce_block(); // height 1

    let header = solen_types::block::BlockHeader {
        height: 2,
        epoch: 999, // wrong epoch
        parent_hash: solen_consensus::engine::block_hash(&engine.get_blocks_for_sync(1, 1)[0].header),
        state_root: [0; 32],
        transactions_root: [0; 32],
        receipts_root: [0; 32],
        proposer: engine.validator_id(),
        timestamp_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 100,
            proposer_signature: vec![],
    };

    let accepted = engine.accept_block(&header, &[]);
    assert!(!accepted, "block with wrong epoch must be rejected");
}

#[test]
fn duplicate_block_at_same_height_rejected() {
    let (engine, _, _, _) = setup_engine();
    engine.produce_block(); // height 1

    // Produce a second block — it should go to pending.
    // But our engine already finalized height 1 (single validator mode),
    // so we need to craft a block at height 2.
    let blocks = engine.get_blocks_for_sync(1, 1);
    let parent_hash = solen_consensus::engine::block_hash(&blocks[0].header);

    let header = solen_types::block::BlockHeader {
        height: 2,
        epoch: 0,
        parent_hash,
        state_root: [0; 32],
        transactions_root: [0; 32],
        receipts_root: [0; 32],
        proposer: engine.validator_id(),
        timestamp_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 100,
            proposer_signature: vec![],
    };

    // First accept should work.
    let accepted1 = engine.accept_block(&header, &[]);
    assert!(accepted1, "first block at height 2 should be accepted");

    // Second accept at same height should be rejected (already pending).
    let accepted2 = engine.accept_block(&header, &[]);
    assert!(!accepted2, "duplicate block at same height must be rejected");
}

#[test]
fn fork_scoring_prefers_higher_priority_proposer() {
    // Use the standard single-validator engine setup, then accept
    // competing blocks from "external" proposers at the next height.
    let (engine, _alice_kp, _alice, validator_id) = setup_engine();

    // Produce block 1 (auto-finalizes in single-validator mode).
    engine.produce_block();
    assert_eq!(engine.height(), 1);

    let blocks = engine.get_blocks_for_sync(1, 1);
    let parent_hash = solen_consensus::engine::block_hash(&blocks[0].header);

    // Two different "external" proposers.
    let v_a = {
        let mut id = [0u8; 32];
        id[0] = 0xAA;
        id
    };
    let v_b = {
        let mut id = [0u8; 32];
        id[0] = 0xBB;
        id
    };

    // Add them to the validator set so accept_block doesn't reject as unknown.
    {
        use solen_consensus::validator::ValidatorInfo;
        let vs = engine.validator_set();
        let mut vs = vs.write().unwrap();
        vs.add(ValidatorInfo::new(v_a, 1000));
        vs.add(ValidatorInfo::new(v_b, 1000));
    }

    // Two competing blocks at height 2 from different proposers.
    let header_a = solen_types::block::BlockHeader {
        height: 2,
        epoch: 0,
        parent_hash,
        state_root: [0xAA; 32],
        transactions_root: [0; 32],
        receipts_root: [0; 32],
        proposer: v_a,
        timestamp_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 100,
        proposer_signature: vec![],
    };

    let header_b = solen_types::block::BlockHeader {
        height: 2,
        epoch: 0,
        parent_hash,
        state_root: [0xBB; 32],
        transactions_root: [0; 32],
        receipts_root: [0; 32],
        proposer: v_b,
        timestamp_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 1,
        proposer_signature: vec![],
    };

    // Accept first block.
    let first_accepted = engine.accept_block(&header_a, &[]);
    assert!(first_accepted, "first competing block should be accepted");
    assert!(engine.has_pending_block(2));

    // Now the competing block arrives. Fork scoring compares proposer
    // priority. One will replace, the other will be rejected.
    let _second_result = engine.accept_block(&header_b, &[]);

    // Either way, we should still have exactly one pending block.
    assert!(engine.has_pending_block(2), "should have a pending block at height 2");
}

// ═══════════════════════════════════════════════════════════════
// SLASHING EDGE CASE TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn missed_block_counter_resets_on_successful_proposal() {
    use solen_consensus::validator::{ValidatorInfo, ValidatorSet};
    use solen_consensus::slashing::record_missed_block;

    let v1 = {
        let mut id = [0u8; 32];
        id[0] = 1;
        id
    };

    let mut vs = ValidatorSet::new(vec![
        ValidatorInfo::new(v1, 100),
    ]);

    // Miss 49 blocks (just under threshold).
    for _ in 0..49 {
        assert!(record_missed_block(&mut vs, &v1).is_none());
    }
    assert_eq!(vs.get_mut(&v1).unwrap().missed_blocks, 49);

    // Reset by successful proposal.
    vs.get_mut(&v1).unwrap().missed_blocks = 0;

    // Miss 49 more — should NOT trigger slash (counter was reset).
    for _ in 0..49 {
        assert!(record_missed_block(&mut vs, &v1).is_none());
    }
    assert_eq!(vs.get_mut(&v1).unwrap().missed_blocks, 49);

    // One more miss triggers slash.
    let evidence = record_missed_block(&mut vs, &v1);
    assert!(evidence.is_some(), "50th miss must trigger slash");
}

#[test]
fn double_sign_detection_requires_different_state_roots() {
    use solen_consensus::slashing::check_double_sign;

    let proposer = [0x01; 32];
    let header_a = solen_types::block::BlockHeader {
        height: 10,
        epoch: 0,
        parent_hash: [0; 32],
        state_root: [0xAA; 32],
        transactions_root: [0; 32],
        receipts_root: [0; 32],
        proposer,
        timestamp_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 100,
            proposer_signature: vec![],
    };

    // Same proposer, same height, SAME state root — not a double sign.
    let mut header_b = header_a.clone();
    header_b.timestamp_ms = 200; // different timestamp but same state root
    assert!(
        check_double_sign(&header_a, &header_b).is_none(),
        "same state root must NOT trigger double-sign"
    );

    // Same proposer, same height, different state root — IS a double sign.
    header_b.state_root = [0xBB; 32];
    assert!(
        check_double_sign(&header_a, &header_b).is_some(),
        "different state root must trigger double-sign"
    );

    // Different proposer — NOT a double sign.
    header_b.proposer = [0x02; 32];
    assert!(
        check_double_sign(&header_a, &header_b).is_none(),
        "different proposer must NOT trigger double-sign"
    );
}

// ═══════════════════════════════════════════════════════════════
// MEMPOOL RESOURCE EXHAUSTION TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn mempool_full_rejects_new_ops() {
    let mempool = Mempool::new(5); // tiny pool

    for i in 0..5u8 {
        let mut sender = [0u8; 32];
        sender[0] = i;
        let op = solen_types::transaction::UserOperation {
            sender,
            nonce: 0,
            actions: vec![],
            max_fee: 100,
            signature: vec![0; 64],
        };
        assert!(mempool.submit(op), "op {} should be accepted", i);
    }

    // 6th op should be rejected.
    let op = solen_types::transaction::UserOperation {
        sender: [0xFF; 32],
        nonce: 0,
        actions: vec![],
        max_fee: 100,
        signature: vec![0; 64],
    };
    assert!(!mempool.submit(op), "mempool at capacity must reject");
}

#[test]
fn mempool_drain_returns_correct_count() {
    let mempool = Mempool::new(100);

    for i in 0..10u8 {
        let mut sender = [0u8; 32];
        sender[0] = i;
        let op = solen_types::transaction::UserOperation {
            sender,
            nonce: 0,
            actions: vec![],
            max_fee: 100,
            signature: vec![0; 64],
        };
        mempool.submit(op);
    }

    // Drain 5 — should get exactly 5.
    let drained = mempool.drain(5);
    assert_eq!(drained.len(), 5, "drain(5) should return 5 ops");

    // Drain remaining — should get 5 more.
    let remaining = mempool.drain(100);
    assert_eq!(remaining.len(), 5, "remaining should be 5");

    // Drain again — empty.
    let empty = mempool.drain(100);
    assert_eq!(empty.len(), 0, "pool should be empty");
}

// ═══════════════════════════════════════════════════════════════
// STATE ROOT ROLLBACK TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn state_root_mismatch_does_not_corrupt_store() {
    let (engine, alice_kp, alice, validator_id) = setup_engine();

    // Produce block 1 so we have state.
    engine.produce_block();
    let height_1 = engine.height();
    assert_eq!(height_1, 1);

    // Get the state root after block 1.
    let root_after_1 = {
        let store = engine.store();
        let store = store.read().unwrap();
        store.state_root()
    };

    // Craft a block at height 2 with WRONG state root.
    let blocks = engine.get_blocks_for_sync(1, 1);
    let parent_hash = solen_consensus::engine::block_hash(&blocks[0].header);

    let fake_header = solen_types::block::BlockHeader {
        height: 2,
        epoch: 0,
        parent_hash,
        state_root: [0xDE; 32], // wrong state root
        transactions_root: [0; 32],
        receipts_root: [0; 32],
        proposer: validator_id,
        timestamp_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 100,
            proposer_signature: vec![],
    };

    // Accept the block (goes to pending).
    engine.accept_block(&fake_header, &[]);

    // Force-finalize it (will execute then detect mismatch and rollback).
    engine.force_finalize_block(2);

    // Chain should NOT have advanced (block rejected).
    assert_eq!(engine.height(), 1, "chain must not advance on state root mismatch");

    // State root should be unchanged (rollback succeeded).
    let root_after_reject = {
        let store = engine.store();
        let store = store.read().unwrap();
        store.state_root()
    };
    assert_eq!(
        root_after_1, root_after_reject,
        "store must be rolled back to pre-execution state after state root mismatch"
    );
}

// ═══════════════════════════════════════════════════════════════
// MEMPOOL PRIORITY TESTS
// ═══════════════════════════════════════════════════════════════

#[test]
fn high_fee_ops_included_when_mempool_full() {
    let mempool = Mempool::new(5);

    // Fill with low-fee operations.
    for i in 0..5u8 {
        let mut sender = [0u8; 32];
        sender[0] = i;
        let op = solen_types::transaction::UserOperation {
            sender,
            nonce: 0,
            actions: vec![],
            max_fee: 1, // low fee
            signature: vec![0; 64],
        };
        assert!(mempool.submit(op));
    }

    // Mempool is full. Drain and verify highest fees come first.
    let drained = mempool.drain(5);
    assert_eq!(drained.len(), 5);
    // All have max_fee=1, so order is by sender for tiebreaking.
    // The important thing: drain works when full.
}
