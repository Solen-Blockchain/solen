//! End-to-end rollup test: register → sequence → batch → prove → verify on L1.
//!
//! Tests the full flow:
//! 1. Register a rollup in the L1 proof verifier registry
//! 2. Submit L2 transactions to the sequencer
//! 3. Produce a batch from the sequencer
//! 4. Generate a mock proof for the state transition
//! 5. Prepare a batch commitment via the publisher
//! 6. Verify the commitment on L1 (proof registry)
//! 7. Submit multiple batches and verify the state root chain holds
//! 8. Reject invalid proofs and stale state roots

use std::sync::Arc;

use solen_crypto::blake3_hash;
use solen_execution::proof::{MockVerifier, ProofVerifierRegistry};
use solen_rollup_kit::batch::BatchPublisher;
use solen_rollup_kit::prover::{MockProver, ProverBackend};
use solen_rollup_kit::sequencer::{L2Transaction, Sequencer, SequencerConfig};

fn dummy_tx(sender_byte: u8, nonce: u64, data: &[u8]) -> L2Transaction {
    let mut sender = [0u8; 32];
    sender[0] = sender_byte;
    L2Transaction {
        sender,
        nonce,
        data: data.to_vec(),
        gas_limit: 100_000,
    }
}

/// Simulate executing a batch and producing a new state root.
/// In a real rollup this would run transactions through the VM.
/// Here we just hash the pre-state with the batch data.
fn execute_batch(pre_state_root: &[u8; 32], batch_data: &[u8]) -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(pre_state_root);
    input.extend_from_slice(batch_data);
    blake3_hash(&input)
}

#[test]
fn full_rollup_lifecycle() {
    let rollup_id = 42u64;
    let genesis_state_root = [0u8; 32];

    // ── 1. Register rollup on L1 ───────────────────────────────
    let mut registry = ProofVerifierRegistry::new();
    registry.register_verifier(Arc::new(MockVerifier));
    registry
        .register_rollup(rollup_id, "mock", genesis_state_root)
        .expect("rollup registration should succeed");

    assert_eq!(
        registry.last_state_root(rollup_id),
        Some(genesis_state_root),
        "initial state root should be genesis"
    );

    // ── 2. Set up sequencer and submit L2 transactions ─────────
    let config = SequencerConfig {
        rollup_id,
        max_batch_size: 10,
        ..Default::default()
    };
    let sequencer = Sequencer::new(config);

    sequencer.submit(dummy_tx(1, 0, b"transfer(alice, bob, 100)")).unwrap();
    sequencer.submit(dummy_tx(2, 0, b"deploy(contract_code)")).unwrap();
    sequencer.submit(dummy_tx(1, 1, b"call(contract, method, args)")).unwrap();

    assert_eq!(sequencer.pending_count(), 3);

    // ── 3. Produce batch from sequencer ────────────────────────
    let batch = sequencer.produce_batch().expect("should produce batch");
    assert_eq!(batch.rollup_id, rollup_id);
    assert_eq!(batch.batch_index, 1);
    assert_eq!(batch.transactions.len(), 3);
    assert_eq!(sequencer.pending_count(), 0);

    // ── 4. Execute batch and compute state transition ──────────
    let batch_data = BatchPublisher::compress_batch(&batch).unwrap();
    let pre_state_root = genesis_state_root;
    let post_state_root = execute_batch(&pre_state_root, &batch_data);
    assert_ne!(post_state_root, pre_state_root, "state should change after execution");

    // ── 5. Generate mock proof ─────────────────────────────────
    let prover = MockProver;
    let proof = prover
        .generate_proof(&pre_state_root, &post_state_root, &batch_data)
        .expect("proof generation should succeed");
    assert_eq!(proof.len(), 32);

    // Verify the proof locally before submitting.
    let data_hash = blake3_hash(&batch_data);
    assert!(
        prover.verify_proof(&pre_state_root, &post_state_root, &data_hash, &proof).unwrap(),
        "local proof verification should pass"
    );

    // ── 6. Prepare commitment via publisher ────────────────────
    let publisher = BatchPublisher::new(rollup_id);
    let commitment = publisher
        .prepare_commitment(&batch, pre_state_root, post_state_root, proof)
        .expect("commitment preparation should succeed");

    assert_eq!(commitment.rollup_id, rollup_id);
    assert_eq!(commitment.batch_index, 1);
    assert_eq!(commitment.state_root, post_state_root);
    assert_eq!(commitment.data_hash, data_hash);

    // ── 7. Verify commitment on L1 ────────────────────────────
    let verified = registry
        .verify_batch(&commitment)
        .expect("L1 verification should not error");
    assert!(verified, "batch 1 should verify successfully");

    assert_eq!(
        registry.last_state_root(rollup_id),
        Some(post_state_root),
        "L1 state root should advance to post-state after verification"
    );

    // ── 8. Submit a second batch (state chain) ─────────────────
    sequencer.submit(dummy_tx(3, 0, b"transfer(charlie, dave, 50)")).unwrap();
    sequencer.submit(dummy_tx(4, 0, b"stake(validator, 1000)")).unwrap();

    let batch2 = sequencer.produce_batch().expect("should produce batch 2");
    assert_eq!(batch2.batch_index, 2);

    let batch2_data = BatchPublisher::compress_batch(&batch2).unwrap();
    let pre_state_2 = post_state_root; // chain from batch 1
    let post_state_2 = execute_batch(&pre_state_2, &batch2_data);

    let proof2 = prover
        .generate_proof(&pre_state_2, &post_state_2, &batch2_data)
        .unwrap();

    let commitment2 = publisher
        .prepare_commitment(&batch2, pre_state_2, post_state_2, proof2)
        .unwrap();

    let verified2 = registry.verify_batch(&commitment2).unwrap();
    assert!(verified2, "batch 2 should verify (state chains from batch 1)");

    assert_eq!(
        registry.last_state_root(rollup_id),
        Some(post_state_2),
        "L1 state root should advance to batch 2 post-state"
    );

    // ── 9. Submit a third batch ────────────────────────────────
    sequencer.submit(dummy_tx(5, 0, b"mint(nft, token_id_1)")).unwrap();
    let batch3 = sequencer.produce_batch().unwrap();
    assert_eq!(batch3.batch_index, 3);

    let batch3_data = BatchPublisher::compress_batch(&batch3).unwrap();
    let pre_state_3 = post_state_2;
    let post_state_3 = execute_batch(&pre_state_3, &batch3_data);
    let proof3 = prover.generate_proof(&pre_state_3, &post_state_3, &batch3_data).unwrap();
    let commitment3 = publisher.prepare_commitment(&batch3, pre_state_3, post_state_3, proof3).unwrap();

    assert!(registry.verify_batch(&commitment3).unwrap(), "batch 3 should verify");
    assert_eq!(registry.last_state_root(rollup_id), Some(post_state_3));
}

#[test]
fn invalid_proof_rejected_on_l1() {
    let rollup_id = 1;
    let mut registry = ProofVerifierRegistry::new();
    registry.register_verifier(Arc::new(MockVerifier));
    registry.register_rollup(rollup_id, "mock", [0u8; 32]).unwrap();

    let sequencer = Sequencer::new(SequencerConfig {
        rollup_id,
        ..Default::default()
    });
    sequencer.submit(dummy_tx(1, 0, b"data")).unwrap();
    let batch = sequencer.produce_batch().unwrap();

    let batch_data = BatchPublisher::compress_batch(&batch).unwrap();
    let post_state = execute_batch(&[0u8; 32], &batch_data);

    // Use a completely wrong proof.
    let bad_proof = vec![0xDE; 32];

    let publisher = BatchPublisher::new(rollup_id);
    let commitment = publisher
        .prepare_commitment(&batch, [0u8; 32], post_state, bad_proof)
        .unwrap();

    let verified = registry.verify_batch(&commitment).unwrap();
    assert!(!verified, "invalid proof should be rejected");

    // State root should NOT advance.
    assert_eq!(
        registry.last_state_root(rollup_id),
        Some([0u8; 32]),
        "state root should remain at genesis after rejected proof"
    );
}

#[test]
fn stale_pre_state_rejected() {
    let rollup_id = 1;
    let prover = MockProver;

    let mut registry = ProofVerifierRegistry::new();
    registry.register_verifier(Arc::new(MockVerifier));
    registry.register_rollup(rollup_id, "mock", [0u8; 32]).unwrap();

    // Submit batch 1 successfully.
    let sequencer = Sequencer::new(SequencerConfig {
        rollup_id,
        ..Default::default()
    });
    sequencer.submit(dummy_tx(1, 0, b"tx1")).unwrap();
    let batch1 = sequencer.produce_batch().unwrap();
    let batch1_data = BatchPublisher::compress_batch(&batch1).unwrap();
    let post_state_1 = execute_batch(&[0u8; 32], &batch1_data);
    let proof1 = prover.generate_proof(&[0u8; 32], &post_state_1, &batch1_data).unwrap();

    let publisher = BatchPublisher::new(rollup_id);
    let c1 = publisher.prepare_commitment(&batch1, [0u8; 32], post_state_1, proof1).unwrap();
    assert!(registry.verify_batch(&c1).unwrap());

    // Now try to submit batch 2 with a proof based on genesis (stale pre-state).
    // The registry's pre-state is now post_state_1, not genesis.
    sequencer.submit(dummy_tx(2, 0, b"tx2")).unwrap();
    let batch2 = sequencer.produce_batch().unwrap();
    let batch2_data = BatchPublisher::compress_batch(&batch2).unwrap();
    let post_state_2_wrong = execute_batch(&[0u8; 32], &batch2_data); // wrong: uses genesis

    // Generate proof against genesis (wrong pre-state).
    let proof2_stale = prover
        .generate_proof(&[0u8; 32], &post_state_2_wrong, &batch2_data)
        .unwrap();
    let c2 = publisher
        .prepare_commitment(&batch2, [0u8; 32], post_state_2_wrong, proof2_stale)
        .unwrap();

    // This should fail because the registry expects pre_state = post_state_1.
    let verified = registry.verify_batch(&c2).unwrap();
    assert!(
        !verified,
        "proof based on stale pre-state (genesis instead of batch 1 post-state) should be rejected"
    );

    // State root should remain at batch 1.
    assert_eq!(registry.last_state_root(rollup_id), Some(post_state_1));
}

#[test]
fn unregistered_rollup_rejected() {
    let mut registry = ProofVerifierRegistry::new();
    registry.register_verifier(Arc::new(MockVerifier));
    // Don't register rollup 99.

    let commitment = solen_types::rollup::BatchCommitment {
        rollup_id: 99,
        batch_index: 1,
        state_root: [1u8; 32],
        data_hash: [2u8; 32],
        proof: vec![0u8; 32],
    };

    let result = registry.verify_batch(&commitment);
    assert!(result.is_err(), "unregistered rollup should error");
}

#[test]
fn multiple_rollups_independent_state_chains() {
    let prover = MockProver;
    let mut registry = ProofVerifierRegistry::new();
    registry.register_verifier(Arc::new(MockVerifier));
    registry.register_rollup(1, "mock", [0u8; 32]).unwrap();
    registry.register_rollup(2, "mock", [0u8; 32]).unwrap();

    // Rollup 1: batch 1
    let seq1 = Sequencer::new(SequencerConfig { rollup_id: 1, ..Default::default() });
    seq1.submit(dummy_tx(1, 0, b"rollup1_tx")).unwrap();
    let batch1 = seq1.produce_batch().unwrap();
    let data1 = BatchPublisher::compress_batch(&batch1).unwrap();
    let post1 = execute_batch(&[0u8; 32], &data1);
    let proof1 = prover.generate_proof(&[0u8; 32], &post1, &data1).unwrap();
    let pub1 = BatchPublisher::new(1);
    let c1 = pub1.prepare_commitment(&batch1, [0u8; 32], post1, proof1).unwrap();
    assert!(registry.verify_batch(&c1).unwrap());

    // Rollup 2: batch 1 (independent state chain)
    let seq2 = Sequencer::new(SequencerConfig { rollup_id: 2, ..Default::default() });
    seq2.submit(dummy_tx(2, 0, b"rollup2_tx")).unwrap();
    let batch2 = seq2.produce_batch().unwrap();
    let data2 = BatchPublisher::compress_batch(&batch2).unwrap();
    let post2 = execute_batch(&[0u8; 32], &data2);
    let proof2 = prover.generate_proof(&[0u8; 32], &post2, &data2).unwrap();
    let pub2 = BatchPublisher::new(2);
    let c2 = pub2.prepare_commitment(&batch2, [0u8; 32], post2, proof2).unwrap();
    assert!(registry.verify_batch(&c2).unwrap());

    // State roots are independent.
    assert_eq!(registry.last_state_root(1), Some(post1));
    assert_eq!(registry.last_state_root(2), Some(post2));
    assert_ne!(post1, post2, "different tx data should produce different state roots");

    // Rollup 1: batch 2 (chains from rollup 1's state, not rollup 2's)
    seq1.submit(dummy_tx(1, 1, b"rollup1_tx2")).unwrap();
    let batch1b = seq1.produce_batch().unwrap();
    let data1b = BatchPublisher::compress_batch(&batch1b).unwrap();
    let post1b = execute_batch(&post1, &data1b);
    let proof1b = prover.generate_proof(&post1, &post1b, &data1b).unwrap();
    let c1b = pub1.prepare_commitment(&batch1b, post1, post1b, proof1b).unwrap();
    assert!(registry.verify_batch(&c1b).unwrap());
    assert_eq!(registry.last_state_root(1), Some(post1b));

    // Rollup 2 still at its own state.
    assert_eq!(registry.last_state_root(2), Some(post2));
}

#[test]
fn large_batch_flow() {
    let rollup_id = 10;
    let prover = MockProver;

    let mut registry = ProofVerifierRegistry::new();
    registry.register_verifier(Arc::new(MockVerifier));
    registry.register_rollup(rollup_id, "mock", [0u8; 32]).unwrap();

    let sequencer = Sequencer::new(SequencerConfig {
        rollup_id,
        max_batch_size: 50,
        ..Default::default()
    });

    // Submit 100 transactions — should produce 2 batches.
    for i in 0..100u64 {
        let data = format!("tx_{}", i);
        sequencer.submit(dummy_tx((i % 10) as u8, i / 10, data.as_bytes())).unwrap();
    }
    assert_eq!(sequencer.pending_count(), 100);

    let publisher = BatchPublisher::new(rollup_id);
    let mut current_state = [0u8; 32];

    // Drain all batches and verify each one.
    let mut batch_count = 0;
    while let Some(batch) = sequencer.produce_batch() {
        batch_count += 1;
        let data = BatchPublisher::compress_batch(&batch).unwrap();
        let post_state = execute_batch(&current_state, &data);
        let proof = prover.generate_proof(&current_state, &post_state, &data).unwrap();
        let commitment = publisher.prepare_commitment(&batch, current_state, post_state, proof).unwrap();

        assert!(
            registry.verify_batch(&commitment).unwrap(),
            "batch {} should verify",
            batch_count
        );
        current_state = post_state;
    }

    assert_eq!(batch_count, 2, "100 txs with max_batch_size=50 should produce 2 batches");
    assert_eq!(sequencer.pending_count(), 0);
    assert_eq!(registry.last_state_root(rollup_id), Some(current_state));
}
