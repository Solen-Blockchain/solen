//! In-memory indexed storage for blocks, transactions, and events.

use std::collections::HashMap;

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

/// An indexed event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedEvent {
    pub block_height: u64,
    pub tx_index: usize,
    pub emitter: String,
    pub topic: String,
}

/// In-memory indexed store.
#[derive(Debug, Default)]
pub struct IndexStore {
    pub blocks: Vec<IndexedBlock>,
    pub transactions: Vec<IndexedTx>,
    pub events: Vec<IndexedEvent>,
    /// Account -> list of tx indices.
    pub account_txs: HashMap<String, Vec<usize>>,
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
}
