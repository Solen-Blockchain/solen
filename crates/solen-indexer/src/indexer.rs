//! Block indexer: processes finalized blocks from the consensus engine.

use std::sync::{Arc, RwLock};

use solen_consensus::engine::{ConsensusEngine, FinalizedBlock};
use tracing::debug;

use crate::store::{IndexStore, IndexedBatch, IndexedBlock, IndexedEvent, IndexedIntent, IndexedRollup, IndexedTx};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn u64_from_le_hex(hex_str: &str) -> Result<u64, ()> {
    if hex_str.len() != 16 {
        return Err(());
    }
    let mut bytes = [0u8; 8];
    for (i, chunk) in hex_str.as_bytes().chunks(2).enumerate() {
        let s = std::str::from_utf8(chunk).map_err(|_| ())?;
        bytes[i] = u8::from_str_radix(s, 16).map_err(|_| ())?;
    }
    Ok(u64::from_le_bytes(bytes))
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
        attestation_count: block.attestations.len(),
    };
    // Track proposer stats.
    let proposer_hex = hex(&block.header.proposer);
    if block.header.proposer != [0u8; 32] {
        *store.blocks_proposed.entry(proposer_hex.clone()).or_insert(0) += 1;
        store.last_proposed.insert(proposer_hex, block.header.height);
    }

    store.add_block(block_summary);

    for (i, receipt) in block.result.receipts.iter().enumerate() {
        let mut related_accounts: Vec<String> = Vec::new();

        let events: Vec<IndexedEvent> = receipt
            .events
            .iter()
            .map(|e| {
                // Extract recipient addresses from event data.
                // These event types all have address[32 bytes] + amount[16 bytes].
                if (e.topic == b"transfer"
                    || e.topic == b"epoch_reward"
                    || e.topic == b"delegator_reward"
                    || e.topic == b"delegate"
                    || e.topic == b"undelegate"
                    || e.topic == b"solver_tip")
                    && e.data.len() >= 32
                {
                    let recipient = hex(&e.data[..32]);
                    if !related_accounts.contains(&recipient) {
                        related_accounts.push(recipient);
                    }
                }

                // Also track event emitters as related accounts.
                let emitter_hex = hex(&e.emitter);
                if !related_accounts.contains(&emitter_hex) {
                    related_accounts.push(emitter_hex.clone());
                }

                IndexedEvent {
                    block_height: block.header.height,
                    tx_index: i,
                    emitter: emitter_hex,
                    topic: String::from_utf8_lossy(&e.topic).to_string(),
                    data: hex(&e.data),
                }
            })
            .collect();

        for event in &events {
            store.add_event(event.clone());

            // Track token holders: mint/transfer events from contracts
            // have recipient[32 bytes] in event data.
            if (event.topic == "mint" || event.topic == "transfer")
                && event.data.len() >= 64
                && !event.emitter.starts_with("ffffffffffffffffffffffffffffffff")
            {
                // First 64 hex chars = 32 bytes = recipient account ID.
                let recipient = event.data[..64].to_string();
                store.track_token_holder(&recipient, &event.emitter);
                // Also track the sender for transfers.
                let sender_hex = hex(&receipt.sender);
                store.track_token_holder(&sender_hex, &event.emitter);
                // Add recipient as related account so the tx shows on their page.
                if !related_accounts.contains(&recipient) {
                    related_accounts.push(recipient);
                }
            }

            // Track slash events: add slashed validator and treasury as related accounts.
            if event.topic == "slashed" && event.data.len() >= 64 {
                let slashed_validator = event.data[..64].to_string();
                if !related_accounts.contains(&slashed_validator) {
                    related_accounts.push(slashed_validator);
                }
                let treasury = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff04".to_string();
                if !related_accounts.contains(&treasury) {
                    related_accounts.push(treasury);
                }
            }

            // Track contract deployments.
            if event.topic == "deploy" && event.data.len() >= 64 {
                let contract_id = &event.data[..64];
                store.track_contract(contract_id);
            }

            // Track rollup registrations.
            // Event: topic=rollup_registered, data=rollup_id[8 LE bytes]
            if event.topic == "rollup_registered" && event.data.len() >= 16 {
                // Rollup ID is the first 8 bytes LE in hex (16 hex chars).
                if let Ok(rollup_id) = u64_from_le_hex(&event.data[..16]) {
                    // Try to extract name/proof_type/sequencer from the tx events.
                    // The registration info is stored in the state, but we can
                    // infer from the call args. For now, index what we know.
                    store.add_rollup(IndexedRollup {
                        rollup_id,
                        name: format!("Rollup #{}", rollup_id),
                        proof_type: String::new(),
                        sequencer: receipt.sender.iter().map(|b| format!("{b:02x}")).collect(),
                        genesis_state_root: String::new(),
                        registered_at_height: block.header.height,
                    });
                }
            }

            // Track batch submissions.
            // Event: topic=batch_verified, data=rollup_id[8]+batch_index[8]+state_root[32]+data_hash[32]
            if event.topic == "batch_verified" && event.data.len() >= 160 {
                if let (Ok(rollup_id), Ok(batch_index)) =
                    (u64_from_le_hex(&event.data[..16]), u64_from_le_hex(&event.data[16..32]))
                {
                    store.add_rollup_batch(IndexedBatch {
                        rollup_id,
                        batch_index,
                        state_root: event.data[32..96].to_string(),
                        data_hash: event.data[96..160].to_string(),
                        verified: true,
                        block_height: block.header.height,
                        tx_index: i,
                    });
                }
            }

            // Track intent fulfillment.
            if event.topic == "intent_fulfilled" && event.data.len() >= 16 {
                if let Ok(intent_id) = u64_from_le_hex(&event.data[..16]) {
                    // Find the transfer and tip from sibling events in this receipt.
                    let mut transfer_to = None;
                    let mut transfer_amount = None;
                    let mut solver = None;
                    let mut solver_tip = None;

                    for sibling in &receipt.events {
                        if sibling.topic == b"transfer" && sibling.data.len() >= 48 {
                            transfer_to = Some(hex(&sibling.data[..32]));
                            let amt_bytes = &sibling.data[32..48];
                            let mut val = 0u128;
                            for (j, &b) in amt_bytes.iter().enumerate() {
                                val |= (b as u128) << (j * 8);
                            }
                            transfer_amount = Some(val.to_string());
                        }
                        if sibling.topic == b"solver_tip" && sibling.data.len() >= 48 {
                            solver = Some(hex(&sibling.data[..32]));
                            let amt_bytes = &sibling.data[32..48];
                            let mut val = 0u128;
                            for (j, &b) in amt_bytes.iter().enumerate() {
                                val |= (b as u128) << (j * 8);
                            }
                            solver_tip = Some(val.to_string());
                        }
                    }

                    store.add_fulfilled_intent(IndexedIntent {
                        intent_id,
                        sender: hex(&receipt.sender),
                        block_height: block.header.height,
                        tx_index: i,
                        transfer_to,
                        transfer_amount,
                        solver_tip,
                        solver,
                    });
                }
            }
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
        store.add_tx(tx, &related_accounts);
    }

    debug!(height = block.header.height, "indexed block");
}

/// Run the indexer as a background task, polling the consensus engine for new blocks.
/// On startup, replays persisted blocks from the state store so historical
/// data is available in the explorer.
pub async fn run_indexer(
    engine: Arc<ConsensusEngine>,
    index_store: Arc<RwLock<IndexStore>>,
    cancel: tokio::sync::watch::Receiver<bool>,
) {
    // Replay persisted blocks from previous sessions in batches to limit memory.
    {
        let batch_size = 1000;
        let mut from = 1u64;
        let mut total_replayed = 0usize;
        loop {
            let batch = engine.load_persisted_blocks_range(from, batch_size);
            if batch.is_empty() {
                break;
            }
            let count = batch.len();
            {
                let mut store = index_store.write().unwrap();
                for block in &batch {
                    index_block(&mut store, block);
                }
            }
            total_replayed += count;
            from += count as u64;
            if count < batch_size {
                break;
            }
        }
        if total_replayed > 0 {
            tracing::info!(blocks = total_replayed, "replayed persisted blocks into indexer");
        }
    }

    let mut last_indexed = engine.height();
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
