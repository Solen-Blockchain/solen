//! Block indexer: processes finalized blocks from the consensus engine.

use std::sync::{Arc, RwLock};

use solen_consensus::engine::{ConsensusEngine, FinalizedBlock};
use tracing::debug;

use crate::store::{IndexStore, IndexedBlock, IndexedEvent, IndexedTx};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Indexes a finalized block into the store.
pub fn index_block(store: &mut IndexStore, block: &FinalizedBlock) {
    let block_summary = IndexedBlock {
        height: block.header.height,
        epoch: block.header.epoch,
        parent_hash: hex(&block.header.parent_hash),
        state_root: hex(&block.header.state_root),
        proposer: hex(&block.header.proposer),
        timestamp_ms: block.header.timestamp_ms,
        tx_count: block.result.receipts.len(),
        gas_used: block.result.gas_used,
    };
    store.add_block(block_summary);

    for (i, receipt) in block.result.receipts.iter().enumerate() {
        let events: Vec<IndexedEvent> = receipt
            .events
            .iter()
            .map(|e| {
                let ie = IndexedEvent {
                    block_height: block.header.height,
                    tx_index: i,
                    emitter: hex(&e.emitter),
                    topic: String::from_utf8_lossy(&e.topic).to_string(),
                };
                ie
            })
            .collect();

        for event in &events {
            store.add_event(event.clone());
        }

        let tx = IndexedTx {
            block_height: block.header.height,
            index: i,
            sender: hex(&receipt.sender),
            nonce: receipt.nonce,
            success: receipt.success,
            gas_used: receipt.gas_used,
            error: receipt.error.clone(),
            events,
        };
        store.add_tx(tx);
    }

    debug!(height = block.header.height, "indexed block");
}

/// Run the indexer as a background task, polling the consensus engine for new blocks.
pub async fn run_indexer(
    engine: Arc<ConsensusEngine>,
    index_store: Arc<RwLock<IndexStore>>,
    cancel: tokio::sync::watch::Receiver<bool>,
) {
    let mut last_indexed = 0u64;
    let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(500));

    loop {
        interval.tick().await;

        if *cancel.borrow() {
            break;
        }

        let current_height = engine.height();
        while last_indexed < current_height {
            last_indexed += 1;
            if let Some(block) = engine.get_block(last_indexed) {
                let mut store = index_store.write().unwrap();
                index_block(&mut store, &block);
            }
        }
    }
}
