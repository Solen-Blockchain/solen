//! Core consensus engine.
//!
//! Implements a simplified Tendermint-style BFT protocol:
//! - Round-robin block proposers
//! - 2/3+ stake-weighted attestation quorum for finality
//! - Epoch-based reward distribution
//! - Slashing for double-sign and downtime

use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use solen_crypto::blake3_hash;
use solen_execution::executor::BlockExecutor;
use solen_execution::receipt::BlockResult;
use solen_storage::StateStore;
use solen_types::block::BlockHeader;
use solen_types::{BlockHeight, Hash, ValidatorId};
use thiserror::Error;
use tokio::time::{interval, Duration};
use tracing::info;

use crate::epoch::EpochManager;
use crate::mempool::Mempool;
use crate::validator::{ValidatorInfo, ValidatorSet};

#[derive(Debug, Error)]
pub enum ConsensusError {
    #[error("engine already running")]
    AlreadyRunning,
    #[error("engine stopped")]
    Stopped,
    #[error("not the proposer for this height")]
    NotProposer,
}

/// Configuration for the consensus engine.
#[derive(Clone)]
pub struct EngineConfig {
    pub block_time_ms: u64,
    pub max_ops_per_block: usize,
    pub validator_id: ValidatorId,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            block_time_ms: 2000,
            max_ops_per_block: 100,
            validator_id: [0u8; 32],
        }
    }
}

/// A finalized block with header, execution result, and attestations.
#[derive(Debug, Clone)]
pub struct FinalizedBlock {
    pub header: BlockHeader,
    pub result: BlockResult,
    pub attestations: Vec<Attestation>,
}

/// A validator's attestation of a block.
#[derive(Debug, Clone)]
pub struct Attestation {
    pub validator_id: ValidatorId,
    pub block_height: u64,
    pub block_hash: Hash,
}

/// The consensus engine manages block production, validation, and finality.
pub struct ConsensusEngine {
    config: EngineConfig,
    store: Arc<RwLock<Box<dyn StateStore>>>,
    mempool: Mempool,
    executor: BlockExecutor,
    chain: Arc<RwLock<Vec<FinalizedBlock>>>,
    validator_set: Arc<RwLock<ValidatorSet>>,
    epoch_manager: Arc<RwLock<EpochManager>>,
}

impl ConsensusEngine {
    /// Create with a single validator (backward compatible).
    pub fn new(
        config: EngineConfig,
        store: Box<dyn StateStore>,
        mempool: Mempool,
    ) -> Self {
        let validator_set = ValidatorSet::new(vec![
            ValidatorInfo::new(config.validator_id, 1000),
        ]);
        Self::with_validators(config, store, mempool, validator_set)
    }

    /// Create with a multi-validator set.
    pub fn with_validators(
        config: EngineConfig,
        store: Box<dyn StateStore>,
        mempool: Mempool,
        validator_set: ValidatorSet,
    ) -> Self {
        Self {
            config,
            store: Arc::new(RwLock::new(store)),
            mempool,
            executor: BlockExecutor::new(),
            chain: Arc::new(RwLock::new(Vec::new())),
            validator_set: Arc::new(RwLock::new(validator_set)),
            epoch_manager: Arc::new(RwLock::new(EpochManager::new())),
        }
    }

    pub fn store(&self) -> Arc<RwLock<Box<dyn StateStore>>> {
        self.store.clone()
    }

    pub fn mempool(&self) -> &Mempool {
        &self.mempool
    }

    pub fn chain(&self) -> Arc<RwLock<Vec<FinalizedBlock>>> {
        self.chain.clone()
    }

    pub fn validator_set(&self) -> Arc<RwLock<ValidatorSet>> {
        self.validator_set.clone()
    }

    pub fn height(&self) -> BlockHeight {
        let chain = self.chain.read().unwrap();
        chain.last().map(|b| b.header.height).unwrap_or(0)
    }

    pub fn get_block(&self, height: BlockHeight) -> Option<FinalizedBlock> {
        let chain = self.chain.read().unwrap();
        chain.iter().find(|b| b.header.height == height).cloned()
    }

    pub fn latest_block(&self) -> Option<FinalizedBlock> {
        let chain = self.chain.read().unwrap();
        chain.last().cloned()
    }

    /// Produce a single block. In multi-validator mode, only the designated
    /// proposer should call this; other validators attest.
    pub fn produce_block(&self) -> FinalizedBlock {
        let ops = self.mempool.drain(self.config.max_ops_per_block);
        let op_count = ops.len();

        let mut store = self.store.write().unwrap();
        let result = self.executor.execute_block(store.as_mut(), &ops);

        let (parent_hash, height) = {
            let chain = self.chain.read().unwrap();
            let parent = chain
                .last()
                .map(|b| block_hash(&b.header))
                .unwrap_or([0u8; 32]);
            let h = chain.last().map(|b| b.header.height + 1).unwrap_or(1);
            (parent, h)
        };

        let epoch = {
            let em = self.epoch_manager.read().unwrap();
            em.epoch_for_height(height)
        };

        let header = BlockHeader {
            height,
            epoch,
            parent_hash,
            state_root: result.state_root,
            transactions_root: compute_tx_root(&ops),
            receipts_root: compute_receipts_root(&result),
            proposer: self.config.validator_id,
            timestamp_ms: now_ms(),
        };

        let bh = block_hash(&header);

        // In single-validator or local mode, all validators auto-attest.
        let attestations = {
            let vs = self.validator_set.read().unwrap();
            vs.active()
                .iter()
                .map(|v| Attestation {
                    validator_id: v.id,
                    block_height: height,
                    block_hash: bh,
                })
                .collect::<Vec<_>>()
        };

        let block = FinalizedBlock {
            header: header.clone(),
            result,
            attestations,
        };

        self.chain.write().unwrap().push(block.clone());

        // Check for epoch transition.
        {
            let mut em = self.epoch_manager.write().unwrap();
            if em.is_epoch_boundary(height) {
                let mut vs = self.validator_set.write().unwrap();
                em.process_epoch_transition(&mut vs);
            }
        }

        info!(
            height,
            ops = op_count,
            gas = block.result.gas_used,
            epoch,
            "block finalized"
        );

        block
    }

    /// Check if this node is the proposer for the next block.
    pub fn is_next_proposer(&self) -> bool {
        let next_height = self.height() + 1;
        let vs = self.validator_set.read().unwrap();
        vs.proposer_for_height(next_height)
            .map(|id| id == self.config.validator_id)
            .unwrap_or(false)
    }

    /// Run the block production loop.
    pub async fn run(&self, cancel: tokio::sync::watch::Receiver<bool>) {
        let mut tick = interval(Duration::from_millis(self.config.block_time_ms));

        {
            let vs = self.validator_set.read().unwrap();
            info!(
                block_time_ms = self.config.block_time_ms,
                validators = vs.active_count(),
                total_stake = %vs.total_active_stake(),
                "consensus engine started"
            );
        }

        loop {
            tick.tick().await;

            if *cancel.borrow() {
                info!("consensus engine stopping");
                break;
            }

            // In a real multi-validator setup, only propose if we're the proposer.
            // For devnet, we always produce.
            self.produce_block();
        }
    }
}

/// Hash a block header to get the block hash.
pub fn block_hash(header: &BlockHeader) -> Hash {
    let serialized = serde_json::to_vec(header).unwrap_or_default();
    blake3_hash(&serialized)
}

fn compute_tx_root(ops: &[solen_types::transaction::UserOperation]) -> Hash {
    if ops.is_empty() {
        return [0u8; 32];
    }
    let serialized = serde_json::to_vec(ops).unwrap_or_default();
    blake3_hash(&serialized)
}

fn compute_receipts_root(result: &BlockResult) -> Hash {
    if result.receipts.is_empty() {
        return [0u8; 32];
    }
    let serialized = serde_json::to_vec(&result.receipts).unwrap_or_default();
    blake3_hash(&serialized)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use solen_crypto::Keypair;
    use solen_execution::genesis::{apply_genesis, GenesisAccount};
    use solen_storage::MemoryStore;
    use solen_types::account::AuthMethod;
    use solen_types::transaction::{Action, UserOperation};

    fn setup_engine() -> (ConsensusEngine, Keypair, [u8; 32], [u8; 32]) {
        let mut store = MemoryStore::new();
        let kp = Keypair::generate();

        let alice = {
            let mut id = [0u8; 32];
            id[..4].copy_from_slice(b"alic");
            id
        };
        let bob = {
            let mut id = [0u8; 32];
            id[..3].copy_from_slice(b"bob");
            id
        };

        apply_genesis(
            &mut store,
            vec![
                GenesisAccount {
                    id: alice,
                    balance: 100_000,
                    auth_methods: vec![AuthMethod::Ed25519 {
                        public_key: kp.public_key(),
                    }],
                },
                GenesisAccount {
                    id: bob,
                    balance: 1_000,
                    auth_methods: vec![],
                },
            ],
        )
        .unwrap();

        let mempool = Mempool::new(1000);
        let engine = ConsensusEngine::new(EngineConfig::default(), Box::new(store), mempool);

        (engine, kp, alice, bob)
    }

    fn setup_multi_validator_engine() -> ConsensusEngine {
        let store = MemoryStore::new();
        let mempool = Mempool::new(1000);

        let v1 = [1u8; 32];
        let v2 = [2u8; 32];
        let v3 = [3u8; 32];

        let vs = ValidatorSet::new(vec![
            ValidatorInfo::new(v1, 100),
            ValidatorInfo::new(v2, 100),
            ValidatorInfo::new(v3, 100),
        ]);

        let config = EngineConfig {
            validator_id: v1,
            ..Default::default()
        };

        ConsensusEngine::with_validators(config, Box::new(store), mempool, vs)
    }

    #[test]
    fn produce_empty_block() {
        let (engine, _, _, _) = setup_engine();
        let block = engine.produce_block();

        assert_eq!(block.header.height, 1);
        assert_eq!(block.result.receipts.len(), 0);
    }

    #[test]
    fn produce_block_with_transactions() {
        let (engine, kp, alice, bob) = setup_engine();
        let executor = BlockExecutor::new();

        let mut op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer { to: bob, amount: 500 }],
            max_fee: 1000,
            signature: vec![],
        };
        let msg = executor.operation_signing_message(&op);
        op.signature = kp.sign(&msg).to_vec();

        engine.mempool().submit(op);

        let block = engine.produce_block();
        assert_eq!(block.result.receipts.len(), 1);
        assert!(block.result.receipts[0].success);
    }

    #[test]
    fn chain_grows_with_parent_hashes() {
        let (engine, _, _, _) = setup_engine();

        engine.produce_block();
        engine.produce_block();
        engine.produce_block();

        assert_eq!(engine.height(), 3);

        let b2 = engine.get_block(2).unwrap();
        let b3 = engine.get_block(3).unwrap();
        assert_eq!(b3.header.parent_hash, block_hash(&b2.header));
    }

    #[test]
    fn multi_validator_attestations() {
        let engine = setup_multi_validator_engine();
        let block = engine.produce_block();

        // All 3 validators should attest.
        assert_eq!(block.attestations.len(), 3);

        // Verify quorum.
        let vs = engine.validator_set();
        let vs = vs.read().unwrap();
        let attester_ids: Vec<_> = block.attestations.iter().map(|a| a.validator_id).collect();
        assert!(vs.has_quorum(&attester_ids));
    }

    #[test]
    fn multi_validator_produces_blocks() {
        let engine = setup_multi_validator_engine();

        for _ in 0..5 {
            engine.produce_block();
        }

        assert_eq!(engine.height(), 5);
    }
}
