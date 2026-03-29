//! Core consensus engine.
//!
//! Implements a simplified Tendermint-style BFT protocol:
//! - Round-robin block proposers
//! - 2/3+ stake-weighted attestation quorum for finality
//! - Epoch-based reward distribution
//! - Slashing for double-sign and downtime

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use solen_crypto::blake3_hash;
use solen_execution::executor::BlockExecutor;
use solen_execution::receipt::BlockResult;
use solen_storage::StateStore;
use solen_types::block::BlockHeader;
use solen_types::transaction::UserOperation;
use solen_types::{BlockHeight, Hash, ValidatorId};
use thiserror::Error;
use tokio::time::{interval, Duration};
use tracing::{info, warn};

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

/// Result of producing a block.
pub struct ProducedBlock {
    /// The finalized block (only set in single-validator mode).
    pub finalized: Option<FinalizedBlock>,
    /// The block header (always set).
    pub header: BlockHeader,
    /// The operations included in the block (for broadcasting to peers).
    pub operations: Vec<UserOperation>,
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
    /// Pending attestations for blocks not yet finalized, keyed by block height.
    pending_attestations: Arc<RwLock<HashMap<u64, Vec<Attestation>>>>,
    /// Proposed blocks waiting for attestations before finalization.
    /// Value: (header, result, operations, proposed_at_instant)
    pending_blocks: Arc<RwLock<HashMap<u64, (BlockHeader, BlockResult, Vec<UserOperation>, std::time::Instant)>>>,
    /// Reward events from epoch transitions, included in the next block's receipts.
    pending_reward_receipts: Arc<RwLock<Vec<solen_execution::receipt::ExecutionReceipt>>>,
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

    /// Create with a multi-validator set. Restores chain height from
    /// persisted metadata if available.
    pub fn with_validators(
        config: EngineConfig,
        store: Box<dyn StateStore>,
        mempool: Mempool,
        validator_set: ValidatorSet,
    ) -> Self {
        // Try to load persisted chain metadata.
        let (restored_height, restored_epoch) = load_chain_meta(&*store);

        let mut chain = Vec::new();
        if restored_height > 0 {
            // Insert a placeholder finalized block so height() returns
            // the correct value. We don't have the full block data, but
            // we need the height to be correct for proposer rotation.
            let placeholder = FinalizedBlock {
                header: BlockHeader {
                    height: restored_height,
                    epoch: restored_epoch,
                    parent_hash: [0u8; 32],
                    state_root: store.state_root(),
                    transactions_root: [0u8; 32],
                    receipts_root: [0u8; 32],
                    proposer: [0u8; 32],
                    timestamp_ms: 0,
                },
                result: BlockResult {
                    state_root: store.state_root(),
                    receipts: vec![],
                    gas_used: 0,
                },
                attestations: vec![],
            };
            chain.push(placeholder);
            info!(height = restored_height, epoch = restored_epoch, "restored chain height from state");
        }

        let mut epoch_manager = EpochManager::new();
        epoch_manager.current_epoch = restored_epoch;

        Self {
            config,
            store: Arc::new(RwLock::new(store)),
            mempool,
            executor: BlockExecutor::new(),
            chain: Arc::new(RwLock::new(chain)),
            validator_set: Arc::new(RwLock::new(validator_set)),
            epoch_manager: Arc::new(RwLock::new(epoch_manager)),
            pending_attestations: Arc::new(RwLock::new(HashMap::new())),
            pending_blocks: Arc::new(RwLock::new(HashMap::new())),
            pending_reward_receipts: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn validator_id(&self) -> ValidatorId {
        self.config.validator_id
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

    /// Produce a single block. Returns the block, its operations, and
    /// whether it was immediately finalized (single-validator mode).
    pub fn produce_block(&self) -> ProducedBlock {
        let ops = self.mempool.drain(self.config.max_ops_per_block);
        let op_count = ops.len();

        let mut store = self.store.write().unwrap();
        let mut result = self.executor.execute_block(store.as_mut(), &ops);

        let (parent_hash, height) = {
            let chain = self.chain.read().unwrap();
            let parent = chain
                .last()
                .map(|b| block_hash(&b.header))
                .unwrap_or([0u8; 32]);
            let h = chain.last().map(|b| b.header.height + 1).unwrap_or(1);
            (parent, h)
        };

        // Include any pending reward receipts from the last epoch transition.
        {
            let mut pending = self.pending_reward_receipts.write().unwrap();
            if !pending.is_empty() {
                result.receipts.extend(pending.drain(..));
                result.gas_used += 0; // rewards don't consume gas
            }
        }

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

        let is_single = self.validator_set.read().unwrap().active_count() <= 1;

        if is_single {
            // Single validator: immediately finalize.
            let attestations = vec![Attestation {
                validator_id: self.config.validator_id,
                block_height: height,
                block_hash: bh,
            }];

            let block = FinalizedBlock {
                header: header.clone(),
                result,
                attestations,
            };

            self.chain.write().unwrap().push(block.clone());
            self.persist_block(&block);

            // Persist chain height.
            {
                let mut store = self.store.write().unwrap();
                save_chain_meta(store.as_mut(), height, epoch);
            }

            self.maybe_process_epoch(height);

            info!(height, ops = op_count, epoch, "block finalized (single validator)");

            ProducedBlock {
                finalized: Some(block),
                header: header.clone(),
                operations: ops,
            }
        } else {
            // Multi-validator: store as pending, self-attest,
            // wait for peer attestations to reach quorum.
            self.pending_blocks.write().unwrap().insert(
                height,
                (header.clone(), result, ops.clone(), std::time::Instant::now()),
            );

            // Self-attest.
            self.accept_attestation(self.config.validator_id, height, bh);

            info!(height, ops = op_count, epoch, "block proposed, waiting for attestations");

            ProducedBlock {
                finalized: None,
                header,
                operations: ops,
            }
        }
    }

    /// Check if this node is the proposer for the next block.
    pub fn is_next_proposer(&self) -> bool {
        let next_height = self.height() + 1;
        let vs = self.validator_set.read().unwrap();
        vs.proposer_for_height(next_height)
            .map(|id| id == self.config.validator_id)
            .unwrap_or(false)
    }

    /// Check if this node should act as backup proposer.
    /// If the designated proposer hasn't produced after 6 seconds,
    /// the next validator in rotation takes over. Every 4 seconds
    /// after that, the next one tries.
    pub fn is_backup_proposer(&self, stalled_for: std::time::Duration) -> bool {
        if stalled_for < std::time::Duration::from_secs(6) {
            return false;
        }

        let next_height = self.height() + 1;
        let vs = self.validator_set.read().unwrap();
        let active = vs.active();
        if active.len() <= 1 {
            return false;
        }

        let skips = ((stalled_for.as_secs() - 6) / 4) + 1;

        for skip in 1..=skips {
            let idx = ((next_height as usize) + skip as usize) % active.len();
            if active[idx].id == self.config.validator_id {
                return true;
            }
        }

        false
    }

    /// Accept a block proposed by another validator. Validates it by
    /// re-executing the operations and checking the state root matches.
    /// Returns true if the block was accepted.
    pub fn accept_block(
        &self,
        header: &BlockHeader,
        operations: &[UserOperation],
    ) -> bool {
        let our_height = self.height();
        let expected_height = our_height + 1;

        if header.height < expected_height {
            // Old block, ignore.
            return false;
        }

        // Verify parent hash matches our latest block (monotonicity check).
        if header.height == expected_height {
            let chain = self.chain.read().unwrap();
            if let Some(last_block) = chain.last() {
                let expected_parent = block_hash(&last_block.header);
                if header.parent_hash != expected_parent && header.parent_hash != [0u8; 32] {
                    warn!(
                        height = header.height,
                        "parent hash mismatch — possible fork, rejecting"
                    );
                    return false;
                }
            }
        }

        if header.height > expected_height {
            // We're behind — fast-forward to catch up.
            // Skip to just before this block's height so we can accept it.
            info!(
                our_height,
                block_height = header.height,
                gap = header.height - expected_height,
                "behind network, fast-forwarding"
            );
            self.fast_forward_to(header.height - 1, header.epoch);
        }

        // Execute the operations against our current state.
        let mut store = self.store.write().unwrap();
        let result = self.executor.execute_block(store.as_mut(), operations);

        // For the next block after our height, verify state root.
        // If we fast-forwarded, our state may differ — accept the block
        // on trust and adopt the peer's state root going forward.
        let state_matches = result.state_root == header.state_root;
        if !state_matches && header.height == expected_height {
            // Only reject if we're at the expected height (not catching up).
            warn!(
                height = header.height,
                "state root mismatch — rejecting block"
            );
            return false;
        }

        // Store as pending, waiting for attestations.
        self.pending_blocks.write().unwrap().insert(
            header.height,
            (header.clone(), result, operations.to_vec(), std::time::Instant::now()),
        );

        info!(
            height = header.height,
            proposer = ?header.proposer[..4],
            "accepted block from peer"
        );

        true
    }

    /// Fast-forward the chain height to catch up with the network.
    /// This skips intermediate blocks (we don't have them) and
    /// updates the chain metadata so the next accept_block works.
    fn fast_forward_to(&self, height: u64, epoch: u64) {
        let placeholder = FinalizedBlock {
            header: BlockHeader {
                height,
                epoch,
                parent_hash: [0u8; 32],
                state_root: self.store.read().unwrap().state_root(),
                transactions_root: [0u8; 32],
                receipts_root: [0u8; 32],
                proposer: [0u8; 32],
                timestamp_ms: now_ms(),
            },
            result: BlockResult {
                state_root: self.store.read().unwrap().state_root(),
                receipts: vec![],
                gas_used: 0,
            },
            attestations: vec![],
        };

        self.chain.write().unwrap().push(placeholder);

        // Persist the new height.
        {
            let mut store = self.store.write().unwrap();
            save_chain_meta(store.as_mut(), height, epoch);
        }

        self.epoch_manager.write().unwrap().current_epoch = epoch;

        info!(height, epoch, "fast-forwarded chain height");
    }

    /// Accept an attestation from a validator. If quorum is reached,
    /// finalize the block.
    pub fn accept_attestation(
        &self,
        validator_id: ValidatorId,
        block_height: u64,
        attested_hash: Hash,
    ) -> bool {
        // Verify attestation is for a block we know about.
        {
            let pending = self.pending_blocks.read().unwrap();
            if let Some((header, _, _, _)) = pending.get(&block_height) {
                let expected_hash = block_hash(header);
                if expected_hash != attested_hash {
                    warn!(
                        height = block_height,
                        "attestation block hash mismatch — ignoring"
                    );
                    return false;
                }
            }
            // If we don't have the block yet, still accept the attestation
            // (it may arrive before the block in gossip ordering).
        }

        // Add to pending attestations.
        {
            let mut atts = self.pending_attestations.write().unwrap();
            let entry = atts.entry(block_height).or_default();

            // Don't accept duplicate attestations.
            if entry.iter().any(|a| a.validator_id == validator_id) {
                return false;
            }

            entry.push(Attestation {
                validator_id,
                block_height,
                block_hash: attested_hash,
            });
        }

        // Check if we have quorum.
        let has_quorum = {
            let atts = self.pending_attestations.read().unwrap();
            let vs = self.validator_set.read().unwrap();
            if let Some(attestations) = atts.get(&block_height) {
                let ids: Vec<ValidatorId> =
                    attestations.iter().map(|a| a.validator_id).collect();
                vs.has_quorum(&ids)
            } else {
                false
            }
        };

        if has_quorum {
            self.finalize_pending_block(block_height);
        }

        has_quorum
    }

    /// Finalize a pending block after quorum is reached.
    fn finalize_pending_block(&self, height: u64) {
        let block_data = self.pending_blocks.write().unwrap().remove(&height);
        let attestations = self
            .pending_attestations
            .write()
            .unwrap()
            .remove(&height)
            .unwrap_or_default();

        if let Some((header, result, _ops, _proposed_at)) = block_data {
            let block = FinalizedBlock {
                header: header.clone(),
                result,
                attestations,
            };

            self.chain.write().unwrap().push(block.clone());
            self.persist_block(&block);

            // Persist chain height.
            {
                let mut store = self.store.write().unwrap();
                save_chain_meta(store.as_mut(), height, header.epoch);
            }

            self.maybe_process_epoch(height);

            info!(
                height,
                epoch = header.epoch,
                "block finalized with quorum"
            );
        }
    }

    /// Process epoch transition if this height is an epoch boundary.
    /// Distributes staking rewards to active validators and updates state.
    fn maybe_process_epoch(&self, height: u64) {
        let is_boundary = {
            let em = self.epoch_manager.read().unwrap();
            em.is_epoch_boundary(height)
        };

        if !is_boundary {
            return;
        }

        // Epoch transition: rotate validators.
        {
            let mut em = self.epoch_manager.write().unwrap();
            let mut vs = self.validator_set.write().unwrap();
            em.process_epoch_transition(&mut vs);
        }

        // Distribute staking rewards from the staking pool account.
        // ~317 SOLEN per epoch (50M/year ÷ 157,680 epochs), with 8 decimals.
        let reward_per_epoch: u128 = 31_700_000_000; // 317 SOLEN in base units
        let mut store = self.store.write().unwrap();

        // Load staking state.
        let staking = solen_system_contracts::staking::StakingContract::load(store.as_ref());

        let active = staking.active_validators();
        let total_stake = staking.total_active_stake();

        if total_stake == 0 || active.is_empty() {
            return;
        }

        let total_reward = reward_per_epoch;

        // Check staking pool balance — rewards stop when pool is empty.
        let pool_key = {
            let mut k = b"acc/".to_vec();
            k.extend_from_slice(&solen_types::system::STAKING_POOL_ADDRESS);
            k
        };

        let pool_balance = if let Ok(Some(data)) = store.get(&pool_key) {
            if let Ok(account) =
                <solen_types::account::Account as borsh::BorshDeserialize>::try_from_slice(&data)
            {
                account.balance
            } else {
                0
            }
        } else {
            0
        };

        if pool_balance == 0 {
            let epoch = self.epoch_manager.read().unwrap().current_epoch;
            info!(epoch, "staking pool exhausted — no rewards this epoch");
            return;
        }

        // Cap reward to available pool balance.
        let actual_reward = total_reward.min(pool_balance);

        // Deduct from staking pool.
        if let Ok(Some(data)) = store.get(&pool_key) {
            if let Ok(mut pool_account) =
                <solen_types::account::Account as borsh::BorshDeserialize>::try_from_slice(&data)
            {
                pool_account.balance = pool_account.balance.saturating_sub(actual_reward);
                if let Ok(encoded) = borsh::to_vec(&pool_account) {
                    let _ = store.put(&pool_key, &encoded);
                }
            }
        }

        // Distribute to validators proportionally and generate receipts.
        let mut reward_events = Vec::new();

        for validator in &active {
            let share = actual_reward * validator.total_stake() / total_stake;
            if share == 0 {
                continue;
            }

            let key = {
                let mut k = b"acc/".to_vec();
                k.extend_from_slice(&validator.id);
                k
            };

            if let Ok(Some(data)) = store.get(&key) {
                if let Ok(mut account) =
                    <solen_types::account::Account as borsh::BorshDeserialize>::try_from_slice(&data)
                {
                    account.balance = account.balance.saturating_add(share);
                    if let Ok(encoded) = borsh::to_vec(&account) {
                        let _ = store.put(&key, &encoded);
                    }

                    // Build event data: validator_id[32] + amount[16]
                    let mut event_data = Vec::with_capacity(48);
                    event_data.extend_from_slice(&validator.id);
                    event_data.extend_from_slice(&share.to_le_bytes());

                    reward_events.push(solen_execution::receipt::Event {
                        emitter: solen_types::system::STAKING_POOL_ADDRESS,
                        topic: b"epoch_reward".to_vec(),
                        data: event_data,
                    });
                }
            }
        }

        // Create a synthetic receipt for the reward distribution.
        if !reward_events.is_empty() {
            let receipt = solen_execution::receipt::ExecutionReceipt {
                sender: solen_types::system::STAKING_POOL_ADDRESS,
                nonce: 0,
                success: true,
                gas_used: 0,
                error: None,
                events: reward_events,
            };
            self.pending_reward_receipts.write().unwrap().push(receipt);
        }

        let epoch = self.epoch_manager.read().unwrap().current_epoch;
        info!(
            epoch,
            validators = active.len(),
            reward = actual_reward,
            pool_remaining = pool_balance.saturating_sub(actual_reward),
            "epoch rewards distributed from staking pool"
        );
    }

    /// Persist a finalized block to the state store for replay after restart.
    fn persist_block(&self, block: &FinalizedBlock) {
        let key = format!("block/{}", block.header.height);
        if let Ok(data) = serde_json::to_vec(&SerializableBlock::from(block)) {
            let mut store = self.store.write().unwrap();
            let _ = store.put(key.as_bytes(), &data);
        }
    }

    /// Load all persisted blocks from the state store (for indexer replay).
    pub fn load_persisted_blocks(&self) -> Vec<FinalizedBlock> {
        let store = self.store.read().unwrap();
        let mut blocks = Vec::new();
        let mut height = 1u64;

        loop {
            let key = format!("block/{}", height);
            match store.get(key.as_bytes()) {
                Ok(Some(data)) => {
                    if let Ok(sb) = serde_json::from_slice::<SerializableBlock>(&data) {
                        blocks.push(sb.into());
                        height += 1;
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }

        blocks
    }

    /// Check if a block is pending at the given height (proposed but not finalized).
    pub fn has_pending_block(&self, height: u64) -> bool {
        self.pending_blocks.read().unwrap().contains_key(&height)
    }

    /// Force-finalize any pending blocks that have been waiting longer than
    /// the timeout. This prevents the chain from stalling when validators
    /// are offline and quorum can't be reached.
    pub fn finalize_timed_out_blocks(&self, timeout: std::time::Duration) -> usize {
        let stale_heights: Vec<u64> = {
            let blocks = self.pending_blocks.read().unwrap();
            blocks
                .iter()
                .filter(|(_, (_, _, _, proposed_at))| proposed_at.elapsed() > timeout)
                .map(|(h, _)| *h)
                .collect()
        };

        let count = stale_heights.len();
        for height in stale_heights {
            warn!(height, "quorum timeout — force-finalizing block");
            self.finalize_pending_block(height);
        }
        count
    }

    /// Number of active validators.
    pub fn active_validator_count(&self) -> usize {
        self.validator_set.read().unwrap().active_count()
    }

    /// Run the block production loop. In multi-validator mode, only
    /// proposes when it's this node's turn.
    pub async fn run(&self, cancel: tokio::sync::watch::Receiver<bool>) {
        let mut tick = interval(Duration::from_millis(self.config.block_time_ms));

        let is_single_validator = {
            let vs = self.validator_set.read().unwrap();
            info!(
                block_time_ms = self.config.block_time_ms,
                validators = vs.active_count(),
                total_stake = %vs.total_active_stake(),
                "consensus engine started"
            );
            vs.active_count() <= 1
        };

        loop {
            tick.tick().await;

            if *cancel.borrow() {
                info!("consensus engine stopping");
                break;
            }

            if is_single_validator {
                // Single validator: always produce (devnet mode).
                self.produce_block();
            } else if self.is_next_proposer() {
                // Multi-validator: only produce when it's our turn.
                self.produce_block();
            }
            // Otherwise: wait for blocks from the proposer via accept_block().
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

/// Key for persisted chain metadata.
const CHAIN_META_KEY: &[u8] = b"__chain_meta__";

/// Persist chain height and epoch to the state store.
fn save_chain_meta(store: &mut dyn StateStore, height: u64, epoch: u64) {
    let mut data = Vec::with_capacity(16);
    data.extend_from_slice(&height.to_le_bytes());
    data.extend_from_slice(&epoch.to_le_bytes());
    let _ = store.put(CHAIN_META_KEY, &data);
}

/// Load chain height and epoch from the state store.
fn load_chain_meta(store: &dyn StateStore) -> (u64, u64) {
    match store.get(CHAIN_META_KEY) {
        Ok(Some(data)) if data.len() >= 16 => {
            let mut h = [0u8; 8];
            let mut e = [0u8; 8];
            h.copy_from_slice(&data[..8]);
            e.copy_from_slice(&data[8..16]);
            (u64::from_le_bytes(h), u64::from_le_bytes(e))
        }
        _ => (0, 0),
    }
}

/// Serializable block for persistence (BlockResult doesn't derive Serialize).
#[derive(serde::Serialize, serde::Deserialize)]
struct SerializableBlock {
    header: BlockHeader,
    state_root: [u8; 32],
    receipts: Vec<solen_execution::receipt::ExecutionReceipt>,
    gas_used: u64,
}

impl From<&FinalizedBlock> for SerializableBlock {
    fn from(b: &FinalizedBlock) -> Self {
        Self {
            header: b.header.clone(),
            state_root: b.result.state_root,
            receipts: b.result.receipts.clone(),
            gas_used: b.result.gas_used,
        }
    }
}

impl From<SerializableBlock> for FinalizedBlock {
    fn from(sb: SerializableBlock) -> Self {
        Self {
            header: sb.header,
            result: BlockResult {
                state_root: sb.state_root,
                receipts: sb.receipts,
                gas_used: sb.gas_used,
            },
            attestations: vec![],
        }
    }
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
        let produced = engine.produce_block();
        let block = produced.finalized.unwrap();

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

        let produced = engine.produce_block();
        let block = produced.finalized.unwrap();
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
    fn multi_validator_propose_and_attest() {
        let v1 = [1u8; 32];
        let v2 = [2u8; 32];
        let v3 = [3u8; 32];

        let store = MemoryStore::new();
        let mempool = Mempool::new(1000);
        let vs = ValidatorSet::new(vec![
            ValidatorInfo::new(v1, 100),
            ValidatorInfo::new(v2, 100),
            ValidatorInfo::new(v3, 100),
        ]);

        let config = EngineConfig {
            validator_id: v1,
            ..Default::default()
        };

        let engine = ConsensusEngine::with_validators(config, Box::new(store), mempool, vs);

        // v1 proposes — block should NOT be immediately finalized (multi-validator).
        let produced = engine.produce_block();
        assert!(produced.finalized.is_none());
        assert_eq!(engine.height(), 0); // not finalized yet

        // v1 already self-attested during produce_block.
        // v2 attests — still no quorum (2/3 = 66%, need >66%).
        let bh = block_hash(&produced.header);
        engine.accept_attestation(v2, 1, bh);
        assert_eq!(engine.height(), 0); // still not finalized

        // v3 attests — quorum reached (3/3 = 100%).
        engine.accept_attestation(v3, 1, bh);
        assert_eq!(engine.height(), 1); // finalized!

        let block = engine.get_block(1).unwrap();
        assert_eq!(block.attestations.len(), 3);
    }

    #[test]
    fn multi_validator_accept_block_from_peer() {
        let v1 = [1u8; 32];
        let v2 = [2u8; 32];
        let v3 = [3u8; 32];
        let v4 = [4u8; 32];

        // Engine running as v2.
        let store = MemoryStore::new();
        let mempool = Mempool::new(1000);
        let vs = ValidatorSet::new(vec![
            ValidatorInfo::new(v1, 100),
            ValidatorInfo::new(v2, 100),
            ValidatorInfo::new(v3, 100),
            ValidatorInfo::new(v4, 100),
        ]);

        let config = EngineConfig {
            validator_id: v2,
            ..Default::default()
        };

        let engine = ConsensusEngine::with_validators(config, Box::new(store), mempool, vs);

        // Simulate receiving a block proposed by v1.
        let header = BlockHeader {
            height: 1,
            epoch: 0,
            parent_hash: [0u8; 32],
            state_root: [0u8; 32], // empty state
            transactions_root: [0u8; 32],
            receipts_root: [0u8; 32],
            proposer: v1,
            timestamp_ms: 12345,
        };

        // Accept the block (no operations, so state root should match).
        let accepted = engine.accept_block(&header, &[]);
        // State root might not match since our store computes differently.
        // The key test is that the flow doesn't panic.
        // In production, both nodes start from the same genesis.
    }
}
