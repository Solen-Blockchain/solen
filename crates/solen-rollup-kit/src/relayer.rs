//! Bridge relayer: monitors L1 events and relays deposits/withdrawals
//! between L1 and rollup domains.

use serde::{Deserialize, Serialize};
use solen_types::RollupId;
use tracing::info;

/// An event detected by the relayer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RelayerEvent {
    /// A deposit on L1 that should be credited on L2.
    Deposit {
        rollup_id: RollupId,
        recipient: [u8; 32],
        amount: u128,
        l1_block: u64,
    },
    /// A withdrawal finalized on L1 — L2 should mark it complete.
    WithdrawalFinalized {
        rollup_id: RollupId,
        withdrawal_id: u64,
        l1_block: u64,
    },
    /// A new batch commitment published on L1.
    BatchPublished {
        rollup_id: RollupId,
        batch_index: u64,
        state_root: [u8; 32],
    },
}

/// The bridge relayer monitors events and queues relay actions.
pub struct BridgeRelayer {
    rollup_id: RollupId,
    events: Vec<RelayerEvent>,
    last_processed_l1_block: u64,
}

impl BridgeRelayer {
    pub fn new(rollup_id: RollupId) -> Self {
        Self {
            rollup_id,
            events: Vec::new(),
            last_processed_l1_block: 0,
        }
    }

    /// Process an L1 event and queue the corresponding relay action.
    pub fn process_event(&mut self, event: RelayerEvent) {
        info!(rollup_id = self.rollup_id, event = ?event, "relayer processing event");
        self.events.push(event);
    }

    /// Drain all queued events for processing by the L2 sequencer.
    pub fn drain_events(&mut self) -> Vec<RelayerEvent> {
        std::mem::take(&mut self.events)
    }

    /// Update the last processed L1 block height.
    pub fn set_last_processed(&mut self, block: u64) {
        self.last_processed_l1_block = block;
    }

    pub fn last_processed(&self) -> u64 {
        self.last_processed_l1_block
    }

    pub fn pending_count(&self) -> usize {
        self.events.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_and_drain() {
        let mut relayer = BridgeRelayer::new(1);

        relayer.process_event(RelayerEvent::Deposit {
            rollup_id: 1,
            recipient: [1u8; 32],
            amount: 1000,
            l1_block: 50,
        });

        relayer.process_event(RelayerEvent::BatchPublished {
            rollup_id: 1,
            batch_index: 5,
            state_root: [2u8; 32],
        });

        assert_eq!(relayer.pending_count(), 2);

        let events = relayer.drain_events();
        assert_eq!(events.len(), 2);
        assert_eq!(relayer.pending_count(), 0);
    }
}
