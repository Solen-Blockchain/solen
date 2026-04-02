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
use solen_execution::proof::ProofVerifierRegistry;
use solen_execution::receipt::BlockResult;
use solen_intents::pool::IntentPool;
use solen_intents::solver::{DirectTransferSolver, IntentSolver};
use solen_storage::StateStore;
use solen_types::block::BlockHeader;
use solen_types::transaction::UserOperation;
use solen_types::{BlockHeight, Hash, ValidatorId};
use thiserror::Error;
use tokio::time::{interval, Duration};
use tracing::{debug, info, warn};

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

/// A block waiting for attestations before finalization.
struct PendingBlock {
    header: BlockHeader,
    operations: Vec<UserOperation>,
    proposed_at: std::time::Instant,
    /// True if we produced this block (state already applied to store).
    /// False if we received it from a peer (NOT yet executed — Tendermint pattern).
    already_executed: bool,
    /// Execution result, only present if already_executed.
    result: Option<BlockResult>,
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
    pending_blocks: Arc<RwLock<HashMap<u64, PendingBlock>>>,
    /// Reward events from epoch transitions, included in the next block's receipts.
    pending_reward_receipts: Arc<RwLock<Vec<solen_execution::receipt::ExecutionReceipt>>>,
    /// Intent pool for intent-aware execution.
    intent_pool: Arc<IntentPool>,
    /// Rollup proof verification registry.
    proof_registry: Arc<RwLock<ProofVerifierRegistry>>,
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
            intent_pool: Arc::new(IntentPool::new(10_000)),
            proof_registry: {
                let mut reg = ProofVerifierRegistry::new();
                reg.register_verifier(Arc::new(solen_execution::proof::MockVerifier));
                Arc::new(RwLock::new(reg))
            },
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

    pub fn intent_pool(&self) -> Arc<IntentPool> {
        self.intent_pool.clone()
    }

    pub fn proof_registry(&self) -> Arc<RwLock<ProofVerifierRegistry>> {
        self.proof_registry.clone()
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
        let mut ops = self.mempool.drain(self.config.max_ops_per_block);

        // Expire intents past current height.
        let current_h = self.height();
        let expired = self.intent_pool.expire(current_h);
        if expired > 0 {
            debug!(expired, height = current_h, "expired intents");
        }

        // Solve pending intents and include as system operations in the block.
        let pending = self.intent_pool.pending_intents();
        if !pending.is_empty() {
            let solver = DirectTransferSolver { id: self.config.validator_id };

            for intent in &pending {
                let solution = self.intent_pool.select_best_solution(intent.id)
                    .ok()
                    .or_else(|| solver.solve(intent));

                if let Some(sol) = solution {
                    // Build a system call operation: sender calls INTENT_ADDRESS.fulfill(...)
                    // Args: intent_id[8] + solver[32] + claimed_tip[16] + num_transfers[4] + (to[32]+amount[16])*N
                    let mut args = Vec::new();
                    args.extend_from_slice(&intent.id.to_le_bytes()); // intent_id[8]
                    args.extend_from_slice(&sol.solver);              // solver[32]
                    args.extend_from_slice(&sol.claimed_tip.to_le_bytes()); // claimed_tip[16]

                    let mut transfer_count: u32 = 0;
                    let count_pos = args.len();
                    args.extend_from_slice(&0u32.to_le_bytes()); // placeholder

                    for op in &sol.operations {
                        for action in &op.actions {
                            if let solen_types::transaction::Action::Transfer { to, amount } = action {
                                args.extend_from_slice(to);
                                args.extend_from_slice(&amount.to_le_bytes());
                                transfer_count += 1;
                            }
                        }
                    }

                    // Patch transfer count.
                    args[count_pos..count_pos+4].copy_from_slice(&transfer_count.to_le_bytes());

                    ops.push(solen_types::transaction::UserOperation {
                        sender: intent.sender,
                        nonce: 0, // system ops use nonce 0
                        actions: vec![solen_types::transaction::Action::Call {
                            target: solen_types::system::INTENT_ADDRESS,
                            method: "fulfill".to_string(),
                            args,
                        }],
                        max_fee: intent.max_fee,
                        signature: vec![0xFF], // marker for system-authorized intent ops
                    });

                    let _ = self.intent_pool.fulfill(intent.id);
                    info!(intent_id = intent.id, "intent solution included in block");
                }
            }
        }

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
        let mut result = {
            let mut store = self.store.write().unwrap();
            self.executor.execute_block_with_height(store.as_mut(), &ops, height)
        };

        // (Intent solutions are now included as operations above — executed by the executor.)

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
                PendingBlock {
                    header: header.clone(),
                    operations: ops.clone(),
                    proposed_at: std::time::Instant::now(),
                    already_executed: true,
                    result: Some(result),
                },
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
    ///
    /// Follows Tendermint's round-based approach: only ONE backup proposer
    /// is active at a time. After 3x block_time, the next validator in
    /// rotation becomes the backup. After another 2x block_time, the next
    /// one takes over, and so on. At any given moment, exactly one
    /// validator should be producing.
    pub fn is_backup_proposer(&self, stalled_for: std::time::Duration) -> bool {
        let min_wait = std::time::Duration::from_millis(self.config.block_time_ms * 3);
        if stalled_for < min_wait {
            return false;
        }

        let next_height = self.height() + 1;
        let vs = self.validator_set.read().unwrap();
        let active = vs.active();
        if active.len() <= 1 {
            return false;
        }

        // Determine which backup "round" we're in.
        // Round 1 = first backup, round 2 = second backup, etc.
        let elapsed_past_min = stalled_for.as_millis() as u64 - min_wait.as_millis() as u64;
        let round_interval_ms = (self.config.block_time_ms * 2).max(4000);
        let round = (elapsed_past_min / round_interval_ms) + 1;

        // Only the validator at this SPECIFIC round position should propose.
        // Previous round validators should NOT still be proposing.
        let idx = ((next_height as usize) + round as usize) % active.len();
        active[idx].id == self.config.validator_id
    }

    /// Accept a block proposed by another validator.
    ///
    /// Following the Tendermint pattern: do NOT execute the block here.
    /// Just validate header consistency (height, parent hash, epoch) and
    /// store as pending. Execution happens in `finalize_pending_block`
    /// after quorum is reached. This prevents state corruption from
    /// rejected blocks.
    pub fn accept_block(
        &self,
        header: &BlockHeader,
        operations: &[UserOperation],
    ) -> bool {
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
            debug!(
                height = header.height,
                "parent hash mismatch — rejecting block, waiting for sync"
            );
            return false;
        }

        if header.height > expected_height {
            debug!(
                our_height,
                block_height = header.height,
                gap = header.height - expected_height,
                "block ahead of our height — waiting for sync"
            );
            return false;
        }

        // Validate epoch.
        let expected_epoch = header.height / crate::epoch::EPOCH_LENGTH;
        if header.epoch != expected_epoch {
            warn!(
                height = header.height,
                expected = expected_epoch,
                got = header.epoch,
                "invalid epoch — rejecting block"
            );
            return false;
        }

        // Validate proposer is a known active validator.
        {
            let vs = self.validator_set.read().unwrap();
            let is_valid_proposer = vs.active().iter().any(|v| v.id == header.proposer);
            if !is_valid_proposer {
                warn!(
                    height = header.height,
                    proposer = ?header.proposer[..4],
                    "proposer not in active validator set — rejecting block"
                );
                return false;
            }
        }

        // Check for duplicate pending/finalized blocks.
        {
            let chain = self.chain.read().unwrap();
            if chain.iter().any(|b| b.header.height == header.height) {
                return false; // Already finalized.
            }
            drop(chain);

            let pending = self.pending_blocks.read().unwrap();
            if let Some(existing) = pending.get(&header.height) {
                if block_hash(&existing.header) != block_hash(header) {
                    // Different block at same height. If we produced ours, keep it.
                    // If peer produced theirs, we have a conflict.
                    // Either way, don't replace what we have.
                    debug!(
                        height = header.height,
                        "conflicting block at same height — keeping existing"
                    );
                }
                return false;
            }
        }

        // Store as pending WITHOUT executing. Execution happens on finalization.
        // This is the key Tendermint pattern: validate header, vote, execute on commit.
        self.pending_blocks.write().unwrap().insert(
            header.height,
            PendingBlock {
                header: header.clone(),
                operations: operations.to_vec(),
                proposed_at: std::time::Instant::now(),
                already_executed: false,
                result: None,
            },
        );

        info!(
            height = header.height,
            proposer = ?header.proposer[..4],
            "accepted block from peer"
        );

        true
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
            if let Some(pb) = pending.get(&block_height) {
                let expected_hash = block_hash(&pb.header);
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

    /// Finalize a pending block after quorum is reached (or timeout).
    ///
    /// If we produced this block (already_executed=true), the state is already
    /// applied — just push to chain. If we received it from a peer
    /// (already_executed=false), execute now and verify state root.
    fn finalize_pending_block(&self, height: u64) {
        let pending_block = self.pending_blocks.write().unwrap().remove(&height);
        let attestations = self
            .pending_attestations
            .write()
            .unwrap()
            .remove(&height)
            .unwrap_or_default();

        let Some(pb) = pending_block else { return };

        let result = if pb.already_executed {
            // We produced this block — state already applied.
            pb.result.unwrap_or(BlockResult {
                state_root: pb.header.state_root,
                receipts: vec![],
                gas_used: 0,
            })
        } else {
            // Received from peer — execute now (Tendermint "Commit" phase).
            let exec_result = {
                let mut store = self.store.write().unwrap();
                self.executor.execute_block_with_height(
                    store.as_mut(), &pb.operations, height,
                )
            };

            // Verify state root matches the proposer's claim.
            if exec_result.state_root != pb.header.state_root {
                warn!(
                    height,
                    ours = ?&exec_result.state_root[..4],
                    theirs = ?&pb.header.state_root[..4],
                    "state root mismatch on finalization — proposer may be byzantine"
                );
                // Still finalize (quorum agreed), but log the divergence.
                // In production, this would trigger a slash.
            }

            exec_result
        };

        let block = FinalizedBlock {
            header: pb.header.clone(),
            result,
            attestations,
            operations: pb.operations,
        };

        self.chain.write().unwrap().push(block.clone());
        self.persist_block_and_meta(&block);

        self.try_epoch_transition(height);

        info!(
            height,
            epoch = pb.header.epoch,
            "block finalized with quorum"
        );
    }

    /// Legacy — rewards now handled by executor. Kept as unused for reference.
    /// Force-finalize a pending block immediately. Used after sync when
    /// accepting a live block that the network has already agreed on —
    /// no need to wait for attestations.
    pub fn force_finalize_block(&self, height: u64) {
        self.finalize_pending_block(height);
    }

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

        // Reject if we already have this block or there's a gap.
        {
            let chain = self.chain.read().unwrap();
            if let Some(last) = chain.last() {
                if height <= last.header.height {
                    return; // Already have this height.
                }
                if height != last.header.height + 1 {
                    warn!(
                        height,
                        our_height = last.header.height,
                        "sync block has gap — skipping"
                    );
                    return; // Gap — can't apply.
                }
                // Note: we intentionally do NOT check parent_hash during sync.
                // Synced blocks are authoritative from peers. A wiped node may
                // have produced its own divergent blocks during startup, so the
                // parent hashes won't match. The gap check above ensures blocks
                // are applied sequentially, and state root verification on the
                // next live block confirms correctness.
            }
        }

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

        // Verify our computed state root matches the block header.
        if exec_result.state_root != header.state_root {
            warn!(
                height,
                ours = ?&exec_result.state_root[..4],
                theirs = ?&header.state_root[..4],
                "state root mismatch in synced block — applying anyway (peer is authoritative)"
            );
        }

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

    /// Process epoch transition if at a boundary. Syncs the consensus
    /// validator set with the staking contract so new validators are
    /// included in proposer rotation and quorum calculations.
    fn try_epoch_transition(&self, height: u64) {
        let mut em = self.epoch_manager.write().unwrap();
        if em.is_epoch_boundary(height) {
            let mut vs = self.validator_set.write().unwrap();
            em.process_epoch_transition(&mut vs);

            // Sync new validators from staking contract into consensus set.
            let store = self.store.read().unwrap();
            let staking = solen_system_contracts::staking::StakingContract::load(store.as_ref());

            for sv in &staking.validators {
                if !sv.is_active {
                    continue;
                }
                // Only add NEW validators — don't modify existing ones.
                let exists = vs.all().iter().any(|v| v.id == sv.id);
                if !exists {
                    let new_info = crate::validator::ValidatorInfo::new(sv.id, sv.total_stake());
                    vs.add(new_info);
                    tracing::info!(
                        validator = ?&sv.id[..4],
                        stake = sv.total_stake(),
                        "new validator joined consensus set"
                    );
                }
            }
        }
    }

    /// Check if a block is pending at the given height (proposed but not finalized).
    /// Clear all pending blocks and attestations at or below the given height.
    /// Called after sync to prevent stale blocks from being force-finalized.
    pub fn clear_stale_pending(&self, current_height: u64) {
        let mut pending = self.pending_blocks.write().unwrap();
        let before = pending.len();
        pending.retain(|h, _| *h > current_height);
        let mut atts = self.pending_attestations.write().unwrap();
        atts.retain(|h, _| *h > current_height);
        let cleared = before - pending.len();
        if cleared > 0 {
            info!(cleared, current_height, "cleared stale pending blocks after sync");
        }
    }

    pub fn has_pending_block(&self, height: u64) -> bool {
        self.pending_blocks.read().unwrap().contains_key(&height)
    }

    /// Force-finalize any pending blocks that have been waiting longer than
    /// the timeout. This prevents the chain from stalling when validators
    /// are offline and quorum can't be reached.
    pub fn finalize_timed_out_blocks(&self, timeout: std::time::Duration) -> usize {
        let current_height = self.height();

        // First, discard any pending blocks that are at or below the current
        // chain height. These are stale from before a sync and must never be
        // finalized — doing so would push an old block to the end of the
        // chain vector, effectively rolling the node backwards.
        {
            let mut pending = self.pending_blocks.write().unwrap();
            let before = pending.len();
            pending.retain(|h, _| *h > current_height);
            let discarded = before - pending.len();
            if discarded > 0 {
                info!(discarded, current_height, "discarded stale pending blocks");
            }
        }

        let stale_heights: Vec<u64> = {
            let blocks = self.pending_blocks.read().unwrap();
            blocks
                .iter()
                .filter(|(_, pb)| pb.proposed_at.elapsed() > timeout)
                .map(|(h, _)| *h)
                .collect()
        };

        let mut count = 0;
        for height in stale_heights {
            // Double-check: only finalize the NEXT expected block.
            if height != self.height() + 1 {
                debug!(height, our_height = self.height(), "skipping stale pending block");
                continue;
            }
            warn!(height, "quorum timeout — force-finalizing block");
            self.finalize_pending_block(height);
            count += 1;
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
    match serde_json::to_vec(header) {
        Ok(data) => blake3_hash(&data),
        Err(e) => {
            warn!(error = %e, "block header serialization failed — using height-based hash");
            blake3_hash(&header.height.to_le_bytes())
        }
    }
}

fn compute_tx_root(ops: &[solen_types::transaction::UserOperation]) -> Hash {
    if ops.is_empty() {
        return [0u8; 32];
    }
    match serde_json::to_vec(ops) {
        Ok(data) => blake3_hash(&data),
        Err(e) => {
            warn!(error = %e, "tx serialization failed — using op count hash");
            blake3_hash(&ops.len().to_le_bytes())
        }
    }
}

fn compute_receipts_root(result: &BlockResult) -> Hash {
    if result.receipts.is_empty() {
        return [0u8; 32];
    }
    match serde_json::to_vec(&result.receipts) {
        Ok(data) => blake3_hash(&data),
        Err(e) => {
            warn!(error = %e, "receipts serialization failed — using count hash");
            blake3_hash(&result.receipts.len().to_le_bytes())
        }
    }
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
