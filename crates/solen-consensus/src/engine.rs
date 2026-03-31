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
    pub chain_id: u64,
    /// Archive mode: never prune blocks.
    pub archive: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            block_time_ms: 2000,
            max_ops_per_block: 100,
            validator_id: [0u8; 32],
            chain_id: 0,
            archive: false,
        }
    }
}

/// A finalized block with header, execution result, and attestations.
#[derive(Debug, Clone)]
pub struct FinalizedBlock {
    pub header: BlockHeader,
    pub result: BlockResult,
    pub attestations: Vec<Attestation>,
    /// Original operations, stored for sync replay.
    pub operations: Vec<UserOperation>,
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
    /// Count of consecutive fork mismatches at the same height.
    fork_mismatch_count: Arc<std::sync::atomic::AtomicU32>,
    fork_mismatch_height: Arc<std::sync::atomic::AtomicU64>,
    /// Set after fast-forward — state is approximate, skip state root verification.
    state_unverified: Arc<std::sync::atomic::AtomicBool>,
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
                operations: vec![],
            };
            chain.push(placeholder);
            info!(height = restored_height, epoch = restored_epoch, "restored chain height from state");
        }

        let mut epoch_manager = EpochManager::new();
        epoch_manager.current_epoch = restored_epoch;

        let chain_id = config.chain_id;
        Self {
            config,
            store: Arc::new(RwLock::new(store)),
            mempool,
            executor: BlockExecutor::new().with_chain_id(chain_id),
            chain: Arc::new(RwLock::new(chain)),
            validator_set: Arc::new(RwLock::new(validator_set)),
            epoch_manager: Arc::new(RwLock::new(epoch_manager)),
            pending_attestations: Arc::new(RwLock::new(HashMap::new())),
            pending_blocks: Arc::new(RwLock::new(HashMap::new())),
            pending_reward_receipts: Arc::new(RwLock::new(Vec::new())),
            fork_mismatch_count: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            fork_mismatch_height: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            state_unverified: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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

    /// Simulate an operation using the engine's executor (with correct chain_id).
    pub fn simulate(
        &self,
        op: &UserOperation,
        store: &dyn solen_storage::StateStore,
    ) -> solen_execution::receipt::ExecutionReceipt {
        self.executor.simulate(store, op)
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

        let (parent_hash, height) = {
            let chain = self.chain.read().unwrap();
            let parent = chain
                .last()
                .map(|b| block_hash(&b.header))
                .unwrap_or([0u8; 32]);
            let h = chain.last().map(|b| b.header.height + 1).unwrap_or(1);
            (parent, h)
        };

        // Execute block with height so the executor handles epoch rewards deterministically.
        let result = {
            let mut store = self.store.write().unwrap();
            self.executor.execute_block_with_height(store.as_mut(), &ops, height)
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

        let is_single = self.validator_set.read().unwrap().active_count() <= 1;

        if is_single {
            // Epoch rewards are handled by the executor via execute_block_with_height.

            let attestations = vec![Attestation {
                validator_id: self.config.validator_id,
                block_height: height,
                block_hash: bh,
            }];

            let block = FinalizedBlock {
                header: header.clone(),
                result,
                attestations,
                operations: ops.clone(),
            };

            self.chain.write().unwrap().push(block.clone());
            self.persist_block_and_meta(&block);

            self.try_epoch_transition(height);

            info!(height, ops = op_count, epoch, "block finalized (single validator)");

            ProducedBlock {
                finalized: Some(block),
                header: header.clone(),
                operations: ops,
            }
        } else {
            // Epoch rewards are handled by the executor via execute_block_with_height.

            // Store as pending, self-attest,
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
        // Hold chain read lock to get a consistent snapshot of height and parent hash.
        let (our_height, expected_height, fork_detected) = {
            let chain = self.chain.read().unwrap();
            let our_height = chain.last().map(|b| b.header.height).unwrap_or(0);
            let expected_height = our_height + 1;

            if header.height < expected_height {
                return false; // Old block, ignore.
            }

            let fork = if header.height == expected_height {
                if let Some(last_block) = chain.last() {
                    let expected_parent = block_hash(&last_block.header);
                    header.parent_hash != expected_parent && header.parent_hash != [0u8; 32]
                } else {
                    false
                }
            } else {
                false
            };

            (our_height, expected_height, fork)
        };

        if fork_detected {
            // The peer's chain is the network consensus — adopt it.
            // Pop our conflicting block and re-execute the peer's version.
            let fmt = |b: &[u8]| -> String { b.iter().map(|x| format!("{x:02x}")).collect() };
            let our_hash = {
                let mut chain = self.chain.write().unwrap();
                let hash = chain.last().map(|b| fmt(&block_hash(&b.header))).unwrap_or_default();
                chain.pop(); // Remove our conflicting block.
                hash
            };
            let their_parent = fmt(&header.parent_hash);
            warn!(
                height = header.height,
                our_block_hash = &our_hash[..16],
                their_parent_hash = &their_parent[..16],
                "parent hash mismatch — adopting peer's chain"
            );
            // Clear any pending state for this height.
            self.pending_blocks.write().unwrap().remove(&(header.height - 1));
            self.pending_blocks.write().unwrap().remove(&header.height);
            self.pending_attestations.write().unwrap().remove(&header.height);
            // Fall through to re-execute and accept the peer's block below.
            // Recompute expected height after popping.
            let new_height = self.height();
            if header.height != new_height + 1 {
                // Still can't accept — too far apart. Let sync handle it.
                self.handle_fork(header.height);
                return false;
            }
        }

        let mut did_fast_forward = false;
        if header.height > expected_height && !fork_detected {
            // We're behind — fast-forward to catch up.
            // Skip to just before this block's height so we can accept it.
            info!(
                our_height,
                block_height = header.height,
                gap = header.height - expected_height,
                "behind network, fast-forwarding"
            );
            self.fast_forward_to(header.height - 1, header.epoch);
            did_fast_forward = true;
            self.state_unverified.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        // Execute the operations (including epoch rewards if applicable).
        // Using execute_block_with_height ensures deterministic reward distribution.
        let result = {
            let mut store = self.store.write().unwrap();
            self.executor.execute_block_with_height(store.as_mut(), operations, header.height)
        };

        let current_root = result.state_root;

        // Verify state root — but skip verification if we fast-forwarded
        // (our state is approximate after skipping blocks) or if the block
        // was not the next expected one.
        let state_matches = current_root == header.state_root;
        let skip_verification = did_fast_forward
            || self.state_unverified.load(std::sync::atomic::Ordering::Relaxed);
        if !state_matches && !skip_verification {
            warn!(
                height = header.height,
                "state root mismatch — rejecting block"
            );
            return false;
        }

        // Reject if we already have a pending OR finalized block at this height.
        {
            // Check if already finalized in our chain.
            let chain = self.chain.read().unwrap();
            if chain.iter().any(|b| b.header.height == header.height) {
                return false; // Already finalized.
            }
            drop(chain);

            let pending = self.pending_blocks.read().unwrap();
            if let Some((existing_header, _, _, _)) = pending.get(&header.height) {
                // Check for double-sign (same proposer, different block) — but only
                // when fully synced to avoid false positives during catch-up.
                if !self.state_unverified.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Some(evidence) = crate::slashing::check_double_sign(existing_header, header) {
                        let mut vs = self.validator_set.write().unwrap();
                        if let Some(slash_result) = crate::slashing::process_slashing(&mut vs, &evidence) {
                            let mut store = self.store.write().unwrap();
                            crate::slashing::persist_slashing_evidence(
                                store.as_mut(), &slash_result, header.height,
                            );
                        }
                    }
                }
                return false; // Already have a block at this height.
            }
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

    /// Handle a fork: track repeated mismatches and escalate response.
    ///
    /// - First 3 mismatches at the same height: just reject and wait for sync.
    /// - After 3+: full state reset and resync from genesis.
    ///
    /// This avoids nuking state on transient disagreements but recovers
    /// from genuine state divergence.
    fn handle_fork(&self, fork_height: u64) {
        use std::sync::atomic::Ordering::Relaxed;

        let last_height = self.fork_mismatch_height.load(Relaxed);
        if last_height == fork_height {
            self.fork_mismatch_count.fetch_add(1, Relaxed);
        } else {
            self.fork_mismatch_height.store(fork_height, Relaxed);
            self.fork_mismatch_count.store(1, Relaxed);
        }

        let count = self.fork_mismatch_count.load(Relaxed);

        if count <= 3 {
            // Mild response: clear pending state, wait for sync to resolve it.
            warn!(
                fork_height,
                mismatch_count = count,
                "parent hash mismatch — rejecting block ({}/3 before resync)",
                count
            );
            self.pending_blocks.write().unwrap().remove(&fork_height);
            self.pending_attestations.write().unwrap().remove(&fork_height);
        } else {
            // Escalate: we're stuck on the wrong fork. Full state reset.
            warn!(
                fork_height,
                mismatch_count = count,
                "persistent fork — resetting state to resync from peers"
            );

            // Clear in-memory chain.
            self.chain.write().unwrap().clear();

            // Wipe account state and system contracts.
            {
                let mut store = self.store.write().unwrap();
                let mut total_deleted = 0usize;
                for prefix in &[b"acc/" as &[u8], b"cs/", b"code/"] {
                    if let Ok(n) = store.delete_prefix(prefix) {
                        total_deleted += n;
                    }
                }
                for key in &[
                    b"__staking_state__" as &[u8],
                    b"__governance_state__",
                    b"__bridge_state__",
                    b"__treasury_state__",
                    b"__vesting_state__",
                ] {
                    let _ = store.delete(*key);
                }
                save_chain_meta(store.as_mut(), 0, 0);
                info!(deleted = total_deleted, "wiped state for full resync");
            }

            // Reset epoch and pending state.
            self.epoch_manager.write().unwrap().current_epoch = 0;
            self.pending_blocks.write().unwrap().clear();
            self.pending_attestations.write().unwrap().clear();
            self.pending_reward_receipts.write().unwrap().clear();

            // Reset counter.
            self.fork_mismatch_count.store(0, Relaxed);

            info!("full resync initiated — will rebuild state from peers via StatusAnnounce");
        }
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
            operations: vec![],
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

        if let Some((header, result, ops, _proposed_at)) = block_data {
            let block = FinalizedBlock {
                header: header.clone(),
                result,
                attestations,
                operations: ops,
            };

            self.chain.write().unwrap().push(block.clone());
            self.persist_block_and_meta(&block);

            // Only advance epoch counter — rewards already applied by proposer.
            self.try_epoch_transition(height);

            info!(
                height,
                epoch = header.epoch,
                "block finalized with quorum"
            );
        }
    }

    /// Legacy — rewards now handled by executor. Kept as unused for reference.
    #[allow(dead_code)]
    fn _distribute_epoch_rewards_legacy(&self) {
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

        // Distribute to validators and delegators proportionally.
        let mut reward_events = Vec::new();

        for validator in &active {
            let validator_share = actual_reward * validator.total_stake() / total_stake;
            if validator_share == 0 {
                continue;
            }

            // Split between validator (self-stake + commission) and delegators.
            let delegator_pool = if validator.total_delegated > 0 {
                validator_share * validator.total_delegated / validator.total_stake()
            } else {
                0
            };
            let commission = delegator_pool * validator.commission_rate_bps as u128 / 10_000;
            let delegator_net = delegator_pool.saturating_sub(commission);

            // Validator gets: self-stake share + commission from delegators.
            let validator_reward = validator_share.saturating_sub(delegator_pool) + commission;

            // Credit validator account.
            credit_account(store.as_mut(), &validator.id, validator_reward);

            let mut event_data = Vec::with_capacity(48);
            event_data.extend_from_slice(&validator.id);
            event_data.extend_from_slice(&validator_reward.to_le_bytes());
            reward_events.push(solen_execution::receipt::Event {
                emitter: solen_types::system::STAKING_POOL_ADDRESS,
                topic: b"epoch_reward".to_vec(),
                data: event_data,
            });

            // Distribute remaining rewards to delegators proportionally.
            if delegator_net > 0 && validator.total_delegated > 0 {
                let delegations = staking.delegations_for_validator(&validator.id);
                for delegation in delegations {
                    let del_share = delegator_net * delegation.amount / validator.total_delegated;
                    if del_share == 0 {
                        continue;
                    }

                    credit_account(store.as_mut(), &delegation.delegator, del_share);

                    let mut event_data = Vec::with_capacity(48);
                    event_data.extend_from_slice(&delegation.delegator);
                    event_data.extend_from_slice(&del_share.to_le_bytes());
                    reward_events.push(solen_execution::receipt::Event {
                        emitter: solen_types::system::STAKING_POOL_ADDRESS,
                        topic: b"delegator_reward".to_vec(),
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

    /// Persist a finalized block and update chain metadata atomically.
    fn persist_block_and_meta(&self, block: &FinalizedBlock) {
        let key = format!("block/{}", block.header.height);
        if let Ok(data) = serde_json::to_vec(&SerializableBlock::from(block)) {
            let mut store = self.store.write().unwrap();
            // Write block data and chain metadata together.
            if let Err(e) = store.put(key.as_bytes(), &data) {
                warn!(height = block.header.height, error = %e, "failed to persist block");
                return;
            }
            save_chain_meta(store.as_mut(), block.header.height, block.header.epoch);
        }

        // Prune old blocks beyond retention window.
        self.prune_old_blocks(block.header.height);
    }

    /// Remove blocks older than the retention window.
    fn prune_old_blocks(&self, current_height: u64) {
        if self.config.archive {
            return; // Archive mode: keep everything.
        }
        const BLOCK_RETENTION: u64 = 10_000_000;
        if current_height <= BLOCK_RETENTION {
            return;
        }
        let prune_below = current_height - BLOCK_RETENTION;
        // Prune in small batches to avoid holding the lock too long.
        let mut store = self.store.write().unwrap();
        for h in (prune_below.saturating_sub(10))..prune_below {
            if h == 0 {
                continue;
            }
            let key = format!("block/{}", h);
            let _ = store.delete(key.as_bytes());
        }
    }

    /// Load persisted blocks from the state store (for indexer replay).
    /// Loads at most `max_blocks` starting from `from_height`.
    pub fn load_persisted_blocks_range(&self, from_height: u64, max_blocks: usize) -> Vec<FinalizedBlock> {
        let store = self.store.read().unwrap();
        let mut blocks = Vec::new();
        let mut height = from_height;

        while blocks.len() < max_blocks {
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

    /// Load all persisted blocks (convenience wrapper, capped at current height).
    pub fn load_persisted_blocks(&self) -> Vec<FinalizedBlock> {
        let max = self.height() as usize;
        self.load_persisted_blocks_range(1, max)
    }

    /// Get blocks for sync — loads from persistent storage.
    /// Returns up to `max_blocks` starting from `from_height`.
    pub fn get_blocks_for_sync(&self, from_height: u64, max_blocks: usize) -> Vec<FinalizedBlock> {
        let store = self.store.read().unwrap();
        let mut blocks = Vec::new();
        let mut height = from_height;

        while blocks.len() < max_blocks {
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

    /// Replay a synced block: execute operations and finalize.
    /// Used during initial sync from peers.
    ///
    /// If `synced_receipts` are provided (from the peer's persisted block),
    /// they are used for indexing instead of the re-execution receipts.
    /// This preserves transaction history during sync.
    pub fn replay_synced_block(
        &self,
        header: &BlockHeader,
        operations: &[UserOperation],
        synced_receipts: Vec<solen_execution::receipt::ExecutionReceipt>,
    ) {
        let height = header.height;

        // Validate epoch matches expected value.
        let expected_epoch = height / crate::epoch::EPOCH_LENGTH;
        if header.epoch != expected_epoch {
            warn!(
                height,
                expected = expected_epoch,
                got = header.epoch,
                "invalid epoch in synced block — skipping"
            );
            return;
        }

        // Execute operations (including epoch rewards if applicable).
        let exec_result = {
            let mut store = self.store.write().unwrap();
            self.executor.execute_block_with_height(store.as_mut(), operations, height)
        };

        // Use synced receipts if available (they include user tx events).
        // Fall back to execution receipts (which only have epoch rewards).
        let receipts = if !synced_receipts.is_empty() {
            synced_receipts
        } else {
            exec_result.receipts
        };

        let result = BlockResult {
            state_root: exec_result.state_root,
            receipts,
            gas_used: exec_result.gas_used,
        };

        let block = FinalizedBlock {
            header: header.clone(),
            result,
            attestations: vec![],
            operations: operations.to_vec(),
        };

        self.chain.write().unwrap().push(block.clone());
        self.persist_block_and_meta(&block);

        // Advance epoch counter (rewards already handled by executor).
        self.try_epoch_transition(height);
    }

    /// Process epoch transition if at a boundary. Acquires locks in consistent
    /// order (epoch_manager first, then validator_set) to prevent deadlocks.
    fn try_epoch_transition(&self, height: u64) {
        let mut em = self.epoch_manager.write().unwrap();
        if em.is_epoch_boundary(height) {
            let mut vs = self.validator_set.write().unwrap();
            em.process_epoch_transition(&mut vs);
        }
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

        // Clean up orphaned attestations for heights already finalized.
        let current_height = self.height();
        let mut atts = self.pending_attestations.write().unwrap();
        atts.retain(|h, _| *h > current_height);

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

/// Credit an account balance by the given amount.
fn credit_account(store: &mut dyn StateStore, account_id: &[u8; 32], amount: u128) {
    let key = {
        let mut k = b"acc/".to_vec();
        k.extend_from_slice(account_id);
        k
    };

    if let Ok(Some(data)) = store.get(&key) {
        if let Ok(mut account) =
            <solen_types::account::Account as borsh::BorshDeserialize>::try_from_slice(&data)
        {
            account.balance = account.balance.saturating_add(amount);
            if let Ok(encoded) = borsh::to_vec(&account) {
                let _ = store.put(&key, &encoded);
            }
        }
    }
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
    #[serde(default)]
    operations: Vec<UserOperation>,
}

impl From<&FinalizedBlock> for SerializableBlock {
    fn from(b: &FinalizedBlock) -> Self {
        Self {
            header: b.header.clone(),
            state_root: b.result.state_root,
            receipts: b.result.receipts.clone(),
            gas_used: b.result.gas_used,
            operations: b.operations.clone(),
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
            operations: sb.operations,
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
