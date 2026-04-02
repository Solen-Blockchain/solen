//! In-memory indexed storage for blocks, transactions, and events.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// An indexed block summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedBlock {
    pub height: u64,
    pub epoch: u64,
    pub parent_hash: String,
    pub state_root: String,
    pub proposer: String,
    pub timestamp_ms: u64,
    pub tx_count: usize,
    pub gas_used: u64,
}

/// An indexed transaction/operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedTx {
    pub block_height: u64,
    pub index: usize,
    pub sender: String,
    pub nonce: u64,
    pub success: bool,
    pub gas_used: u64,
    pub error: Option<String>,
    pub events: Vec<IndexedEvent>,
}

/// Published contract source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractSource {
    pub code_hash: String,
    pub source_code: String,
    pub language: String,
    pub compiler_version: String,
    pub published_at: u64,
    /// Whether the source has been verified to compile to the on-chain bytecode.
    #[serde(default)]
    pub verified: bool,
}

/// An indexed fulfilled intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedIntent {
    pub intent_id: u64,
    pub sender: String,
    pub block_height: u64,
    pub tx_index: usize,
    pub transfer_to: Option<String>,
    pub transfer_amount: Option<String>,
    pub solver_tip: Option<String>,
    pub solver: Option<String>,
}

/// An indexed rollup registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedRollup {
    pub rollup_id: u64,
    pub name: String,
    pub proof_type: String,
    pub sequencer: String,
    pub genesis_state_root: String,
    pub registered_at_height: u64,
}

/// An indexed rollup batch commitment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedBatch {
    pub rollup_id: u64,
    pub batch_index: u64,
    pub state_root: String,
    pub data_hash: String,
    pub verified: bool,
    pub block_height: u64,
    pub tx_index: usize,
}

/// An indexed event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedEvent {
    pub block_height: u64,
    pub tx_index: usize,
    pub emitter: String,
    pub topic: String,
    pub data: String,
}

/// In-memory indexed store.
#[derive(Debug, Default)]
pub struct IndexStore {
    pub blocks: Vec<IndexedBlock>,
    pub transactions: Vec<IndexedTx>,
    pub events: Vec<IndexedEvent>,
    /// Account -> list of tx indices.
    pub account_txs: HashMap<String, Vec<usize>>,
    /// Account -> set of token contract IDs that have sent tokens to this account.
    pub account_tokens: HashMap<String, HashSet<String>>,
    /// Contract → set of accounts that hold tokens from this contract.
    pub token_holders: HashMap<String, HashSet<String>>,
    /// Set of known contract addresses (accounts with code deployed).
    pub contracts: HashSet<String>,
    /// Published contract source code by code_hash.
    pub contract_sources: HashMap<String, ContractSource>,
    /// Fulfilled intents.
    pub fulfilled_intents: Vec<IndexedIntent>,
    /// Registered rollups.
    pub rollups: HashMap<u64, IndexedRollup>,
    /// Rollup batch commitments (rollup_id -> batches).
    pub rollup_batches: HashMap<u64, Vec<IndexedBatch>>,
    /// Blocks proposed per validator.
    pub blocks_proposed: HashMap<String, u64>,
    /// Last block height proposed per validator.
    pub last_proposed: HashMap<String, u64>,
    pub latest_height: u64,
}

impl IndexStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_block(&mut self, block: IndexedBlock) {
        self.latest_height = block.height;
        self.blocks.push(block);
    }

    pub fn add_tx(&mut self, tx: IndexedTx, related_accounts: &[String]) {
        let idx = self.transactions.len();
        self.account_txs
            .entry(tx.sender.clone())
            .or_default()
            .push(idx);
        for account in related_accounts {
            if account != &tx.sender {
                self.account_txs
                    .entry(account.clone())
                    .or_default()
                    .push(idx);
            }
        }
        self.transactions.push(tx);
    }

    pub fn add_event(&mut self, event: IndexedEvent) {
        self.events.push(event);
    }

    pub fn get_block(&self, height: u64) -> Option<&IndexedBlock> {
        self.blocks.iter().find(|b| b.height == height)
    }

    pub fn get_recent_blocks(&self, limit: usize) -> Vec<&IndexedBlock> {
        self.blocks.iter().rev().take(limit).collect()
    }

    pub fn get_recent_blocks_paged(&self, limit: usize, offset: usize) -> Vec<&IndexedBlock> {
        self.blocks.iter().rev().skip(offset).take(limit).collect()
    }

    pub fn get_tx(&self, block_height: u64, index: usize) -> Option<&IndexedTx> {
        self.transactions
            .iter()
            .find(|tx| tx.block_height == block_height && tx.index == index)
    }

    pub fn get_block_txs(&self, block_height: u64) -> Vec<&IndexedTx> {
        self.transactions
            .iter()
            .filter(|tx| tx.block_height == block_height)
            .collect()
    }

    pub fn get_recent_txs(&self, limit: usize) -> Vec<&IndexedTx> {
        self.transactions.iter().rev().take(limit).collect()
    }

    pub fn get_recent_txs_paged(&self, limit: usize, offset: usize) -> Vec<&IndexedTx> {
        self.transactions.iter().rev().skip(offset).take(limit).collect()
    }

    pub fn get_account_txs_paged(&self, account: &str, limit: usize, offset: usize) -> Vec<&IndexedTx> {
        self.account_txs
            .get(account)
            .map(|indices| {
                indices
                    .iter()
                    .rev()
                    .skip(offset)
                    .take(limit)
                    .filter_map(|&i| self.transactions.get(i))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_account_tx_count(&self, account: &str) -> usize {
        self.account_txs.get(account).map(|v| v.len()).unwrap_or(0)
    }

    pub fn get_account_txs(&self, account: &str, limit: usize) -> Vec<&IndexedTx> {
        self.account_txs
            .get(account)
            .map(|indices| {
                indices
                    .iter()
                    .rev()
                    .take(limit)
                    .filter_map(|&i| self.transactions.get(i))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_recent_events(&self, limit: usize) -> Vec<&IndexedEvent> {
        self.events.iter().rev().take(limit).collect()
    }

    pub fn get_recent_events_paged(&self, limit: usize, offset: usize) -> Vec<&IndexedEvent> {
        self.events.iter().rev().skip(offset).take(limit).collect()
    }

    /// Record that an account holds tokens from a contract.
    pub fn track_token_holder(&mut self, account: &str, contract: &str) {
        self.account_tokens
            .entry(account.to_string())
            .or_default()
            .insert(contract.to_string());
        self.token_holders
            .entry(contract.to_string())
            .or_default()
            .insert(account.to_string());
    }

    /// Get all known holders of a token contract.
    pub fn get_token_holders(&self, contract: &str) -> Vec<String> {
        self.token_holders
            .get(contract)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Record a deployed contract.
    pub fn track_contract(&mut self, contract_id: &str) {
        self.contracts.insert(contract_id.to_string());
    }

    /// Get token contracts associated with an account.
    pub fn get_account_tokens(&self, account: &str) -> Vec<String> {
        self.account_tokens
            .get(account)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all known contracts.
    pub fn get_contracts(&self) -> Vec<String> {
        self.contracts.iter().cloned().collect()
    }

    pub fn add_fulfilled_intent(&mut self, intent: IndexedIntent) {
        self.fulfilled_intents.push(intent);
    }

    pub fn get_recent_intents(&self, limit: usize) -> Vec<&IndexedIntent> {
        self.fulfilled_intents.iter().rev().take(limit).collect()
    }

    pub fn add_rollup(&mut self, rollup: IndexedRollup) {
        self.rollups.insert(rollup.rollup_id, rollup);
    }

    pub fn add_rollup_batch(&mut self, batch: IndexedBatch) {
        self.rollup_batches
            .entry(batch.rollup_id)
            .or_default()
            .push(batch);
    }

    pub fn get_rollups(&self) -> Vec<&IndexedRollup> {
        let mut rollups: Vec<_> = self.rollups.values().collect();
        rollups.sort_by_key(|r| r.rollup_id);
        rollups
    }

    pub fn get_rollup(&self, rollup_id: u64) -> Option<&IndexedRollup> {
        self.rollups.get(&rollup_id)
    }

    pub fn get_rollup_batches(&self, rollup_id: u64, limit: usize) -> Vec<&IndexedBatch> {
        self.rollup_batches
            .get(&rollup_id)
            .map(|batches| batches.iter().rev().take(limit).collect())
            .unwrap_or_default()
    }

    pub fn get_rollup_batch_count(&self, rollup_id: u64) -> usize {
        self.rollup_batches.get(&rollup_id).map(|b| b.len()).unwrap_or(0)
    }
}
