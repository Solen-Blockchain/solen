//! End-to-end tests for on-chain rollup committee proof verification.
//!
//! Covers the full wire path: register_rollup (committee config parsing +
//! JSON storage) -> submit_batch (read committee, verify attestations over the
//! pre->post transition). The cryptographic core is unit-tested in
//! solen-rollup-kit::prover; these tests guard the arg encoding / dispatch.

use solen_crypto::Keypair;
use solen_execution::executor::BlockExecutor;
use solen_execution::fees::FeeConfig;
use solen_execution::genesis::{apply_genesis, GenesisAccount};
use solen_storage::MemoryStore;
use solen_types::account::AuthMethod;
use solen_types::system::BRIDGE_ADDRESS;
use solen_types::transaction::{Action, UserOperation};

const D: u128 = 100_000_000; // 1 SOLEN in base units

fn zero_fee_executor() -> BlockExecutor {
    BlockExecutor::with_fee_config(FeeConfig { base_fee_per_gas: 0, ..Default::default() })
}

fn sign_op(kp: &Keypair, executor: &BlockExecutor, op: &mut UserOperation) {
    let msg = executor.operation_signing_message(op);
    op.signature = kp.sign(&msg).to_vec();
}

/// Build register_rollup args for a committee rollup:
/// rollup_id[8] + name_len[4] + name + pt_len[4] + "committee"
///   + sequencer[32] + genesis_root[32] + threshold[4] + num[4] + attestor[32]*N
fn register_args(
    rollup_id: u64,
    name: &str,
    sequencer: &[u8; 32],
    genesis_root: &[u8; 32],
    threshold: u32,
    attestors: &[[u8; 32]],
) -> Vec<u8> {
    let mut a = Vec::new();
    a.extend_from_slice(&rollup_id.to_le_bytes());
    a.extend_from_slice(&(name.len() as u32).to_le_bytes());
    a.extend_from_slice(name.as_bytes());
    let pt = b"committee";
    a.extend_from_slice(&(pt.len() as u32).to_le_bytes());
    a.extend_from_slice(pt);
    a.extend_from_slice(sequencer);
    a.extend_from_slice(genesis_root);
    a.extend_from_slice(&threshold.to_le_bytes());
    a.extend_from_slice(&(attestors.len() as u32).to_le_bytes());
    for at in attestors {
        a.extend_from_slice(at);
    }
    a
}

/// Build submit_batch args: rollup_id[8] + batch_index[8] + state_root[32]
///   + data_hash[32] + proof_len[4] + proof
fn submit_args(
    rollup_id: u64,
    batch_index: u64,
    state_root: &[u8; 32],
    data_hash: &[u8; 32],
    proof: &[u8],
) -> Vec<u8> {
    let mut a = Vec::new();
    a.extend_from_slice(&rollup_id.to_le_bytes());
    a.extend_from_slice(&batch_index.to_le_bytes());
    a.extend_from_slice(state_root);
    a.extend_from_slice(data_hash);
    a.extend_from_slice(&(proof.len() as u32).to_le_bytes());
    a.extend_from_slice(proof);
    a
}

/// Build a committee proof from (index, keypair) signers.
fn committee_proof(
    rollup_id: u64,
    batch_index: u64,
    pre: &[u8; 32],
    post: &[u8; 32],
    data_hash: &[u8; 32],
    signers: &[(u32, &Keypair)],
) -> Vec<u8> {
    let msg = solen_rollup_kit::prover::committee_attestation_message(
        rollup_id, batch_index, pre, post, data_hash,
    );
    let mut proof = (signers.len() as u32).to_le_bytes().to_vec();
    for (idx, kp) in signers {
        proof.extend_from_slice(&idx.to_le_bytes());
        proof.extend_from_slice(&kp.sign(&msg));
    }
    proof
}

fn call_op(seq_kp: &Keypair, seq: [u8; 32], nonce: u64, method: &str, args: Vec<u8>, ex: &BlockExecutor) -> UserOperation {
    let mut op = UserOperation {
        sender: seq,
        nonce,
        actions: vec![Action::Call { target: BRIDGE_ADDRESS, method: method.to_string(), args }],
        max_fee: 0,
        signature: vec![],
    };
    sign_op(seq_kp, ex, &mut op);
    op
}

struct Fixture {
    store: MemoryStore,
    ex: BlockExecutor,
    seq_kp: Keypair,
    seq: [u8; 32],
    attestors_kp: Vec<Keypair>,
    attestors: Vec<[u8; 32]>,
    genesis_root: [u8; 32],
}

fn setup() -> Fixture {
    let seq_kp = Keypair::from_seed(&[0x0A; 32]);
    let seq = seq_kp.public_key();
    let attestors_kp: Vec<Keypair> = (1u8..=3).map(|i| Keypair::from_seed(&[i; 32])).collect();
    let attestors: Vec<[u8; 32]> = attestors_kp.iter().map(|k| k.public_key()).collect();

    let mut store = MemoryStore::new();
    apply_genesis(
        &mut store,
        vec![GenesisAccount {
            id: seq,
            balance: 50_000 * D, // covers the 10,000 SOLEN registration deposit
            auth_methods: vec![AuthMethod::Ed25519 { public_key: seq }],
        }],
    )
    .unwrap();

    Fixture { store, ex: zero_fee_executor(), seq_kp, seq, attestors_kp, attestors, genesis_root: [0u8; 32] }
}

#[test]
fn committee_rollup_accepts_valid_batch_and_rejects_insufficient() {
    let mut f = setup();
    let rid = 7u64;

    // Register a 2-of-3 committee rollup.
    let reg = call_op(&f.seq_kp, f.seq, 0, "register_rollup",
        register_args(rid, "test-rollup", &f.seq, &f.genesis_root, 2, &f.attestors), &f.ex);
    let r = f.ex.execute_block(&mut f.store, &[reg]);
    assert!(r.receipts[0].success, "register_rollup failed: {:?}", r.receipts[0].error);

    let post = [9u8; 32];
    let data_hash = [7u8; 32];

    // Batch 0 with only ONE attestor signature -> below threshold -> rejected.
    let weak = committee_proof(rid, 0, &f.genesis_root, &post, &data_hash, &[(0, &f.attestors_kp[0])]);
    let op = call_op(&f.seq_kp, f.seq, 1, "submit_batch", submit_args(rid, 0, &post, &data_hash, &weak), &f.ex);
    let r = f.ex.execute_block(&mut f.store, &[op]);
    assert!(!r.receipts[0].success, "submit_batch must reject a below-threshold proof");

    // Batch 0 with TWO valid attestor signatures -> accepted. (nonce 2: the
    // rejected submit above still consumed nonce 1 for replay protection.)
    let good = committee_proof(rid, 0, &f.genesis_root, &post, &data_hash,
        &[(0, &f.attestors_kp[0]), (1, &f.attestors_kp[1])]);
    let op = call_op(&f.seq_kp, f.seq, 2, "submit_batch", submit_args(rid, 0, &post, &data_hash, &good), &f.ex);
    let r = f.ex.execute_block(&mut f.store, &[op]);
    assert!(r.receipts[0].success, "submit_batch must accept a valid quorum proof: {:?}", r.receipts[0].error);
}

#[test]
fn committee_rollup_rejects_forged_post_root() {
    let mut f = setup();
    let rid = 7u64;
    let reg = call_op(&f.seq_kp, f.seq, 0, "register_rollup",
        register_args(rid, "r", &f.seq, &f.genesis_root, 2, &f.attestors), &f.ex);
    assert!(f.ex.execute_block(&mut f.store, &[reg]).receipts[0].success);

    // Attestors sign post=A, but the sequencer submits a different post=B.
    let signed_post = [9u8; 32];
    let forged_post = [0xAAu8; 32];
    let data_hash = [7u8; 32];
    let proof = committee_proof(rid, 0, &f.genesis_root, &signed_post, &data_hash,
        &[(0, &f.attestors_kp[0]), (1, &f.attestors_kp[1])]);
    let op = call_op(&f.seq_kp, f.seq, 1, "submit_batch", submit_args(rid, 0, &forged_post, &data_hash, &proof), &f.ex);
    let r = f.ex.execute_block(&mut f.store, &[op]);
    assert!(!r.receipts[0].success, "a post root the committee did not sign must be rejected");
}
