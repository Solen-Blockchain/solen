//! Cross-domain messenger: sends and receives messages between rollup domains.
//!
//! Messages include replay protection (nonce per source-destination pair),
//! timeout semantics, and proof references for verification.

use serde::{Deserialize, Serialize};
use solen_types::{Hash, RollupId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MessengerError {
    #[error("duplicate message nonce {0}")]
    DuplicateNonce(u64),
    #[error("message expired at block {0}")]
    Expired(u64),
    #[error("message not found")]
    NotFound,
    #[error("message already executed")]
    AlreadyExecuted,
}

/// Status of a cross-domain message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageStatus {
    Pending,
    Executed,
    Expired,
    Failed,
}

/// A cross-domain message receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageReceipt {
    pub id: u64,
    pub source: RollupId,
    pub destination: RollupId,
    pub nonce: u64,
    pub sender: [u8; 32],
    pub payload: Vec<u8>,
    pub payload_hash: Hash,
    pub timeout_block: u64,
    pub proof_reference: Hash,
    pub status: MessageStatus,
}

/// Manages cross-domain message sending and receiving.
#[derive(Debug, Default)]
pub struct CrossDomainMessenger {
    messages: Vec<MessageReceipt>,
    /// Nonce counter per (source, destination) pair.
    nonces: std::collections::HashMap<(RollupId, RollupId), u64>,
    next_id: u64,
}

impl CrossDomainMessenger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Send a message from one domain to another.
    pub fn send_message(
        &mut self,
        source: RollupId,
        destination: RollupId,
        sender: [u8; 32],
        payload: Vec<u8>,
        timeout_block: u64,
    ) -> u64 {
        let nonce = self
            .nonces
            .entry((source, destination))
            .or_insert(0);
        let current_nonce = *nonce;
        *nonce += 1;

        let payload_hash = solen_crypto::blake3_hash(&payload);
        let id = self.next_id;
        self.next_id += 1;

        self.messages.push(MessageReceipt {
            id,
            source,
            destination,
            nonce: current_nonce,
            sender,
            payload,
            payload_hash,
            timeout_block,
            proof_reference: [0u8; 32], // set when proven
            status: MessageStatus::Pending,
        });

        id
    }

    /// Mark a message as executed on the destination domain.
    pub fn execute_message(
        &mut self,
        message_id: u64,
        current_block: u64,
    ) -> Result<&MessageReceipt, MessengerError> {
        let msg = self
            .messages
            .iter_mut()
            .find(|m| m.id == message_id)
            .ok_or(MessengerError::NotFound)?;

        if msg.status == MessageStatus::Executed {
            return Err(MessengerError::AlreadyExecuted);
        }

        if current_block > msg.timeout_block {
            msg.status = MessageStatus::Expired;
            return Err(MessengerError::Expired(msg.timeout_block));
        }

        msg.status = MessageStatus::Executed;
        Ok(msg)
    }

    /// Get all pending messages for a destination domain.
    pub fn pending_for_destination(&self, destination: RollupId) -> Vec<&MessageReceipt> {
        self.messages
            .iter()
            .filter(|m| m.destination == destination && m.status == MessageStatus::Pending)
            .collect()
    }

    /// Get a message by ID.
    pub fn get_message(&self, id: u64) -> Option<&MessageReceipt> {
        self.messages.iter().find(|m| m.id == id)
    }

    /// Expire all messages past their timeout.
    pub fn expire_messages(&mut self, current_block: u64) -> usize {
        let mut count = 0;
        for msg in &mut self.messages {
            if msg.status == MessageStatus::Pending && current_block > msg.timeout_block {
                msg.status = MessageStatus::Expired;
                count += 1;
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_and_execute() {
        let mut messenger = CrossDomainMessenger::new();

        let id = messenger.send_message(1, 2, [1u8; 32], b"hello".to_vec(), 1000);
        assert_eq!(id, 0);

        let pending = messenger.pending_for_destination(2);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].payload, b"hello");

        messenger.execute_message(id, 500).unwrap();
        assert_eq!(
            messenger.get_message(id).unwrap().status,
            MessageStatus::Executed
        );
        assert_eq!(messenger.pending_for_destination(2).len(), 0);
    }

    #[test]
    fn expired_message() {
        let mut messenger = CrossDomainMessenger::new();
        let id = messenger.send_message(1, 2, [1u8; 32], b"data".to_vec(), 100);

        let err = messenger.execute_message(id, 200).unwrap_err();
        assert!(matches!(err, MessengerError::Expired(100)));
    }

    #[test]
    fn replay_protection() {
        let mut messenger = CrossDomainMessenger::new();

        let id1 = messenger.send_message(1, 2, [1u8; 32], b"a".to_vec(), 1000);
        let id2 = messenger.send_message(1, 2, [1u8; 32], b"b".to_vec(), 1000);

        // Different IDs and nonces.
        assert_ne!(id1, id2);
        let m1 = messenger.get_message(id1).unwrap();
        let m2 = messenger.get_message(id2).unwrap();
        assert_eq!(m1.nonce, 0);
        assert_eq!(m2.nonce, 1);
    }

    #[test]
    fn double_execute_rejected() {
        let mut messenger = CrossDomainMessenger::new();
        let id = messenger.send_message(1, 2, [1u8; 32], b"x".to_vec(), 1000);

        messenger.execute_message(id, 50).unwrap();
        let err = messenger.execute_message(id, 60).unwrap_err();
        assert!(matches!(err, MessengerError::AlreadyExecuted));
    }

    #[test]
    fn bulk_expire() {
        let mut messenger = CrossDomainMessenger::new();
        messenger.send_message(1, 2, [1u8; 32], b"a".to_vec(), 100);
        messenger.send_message(1, 2, [1u8; 32], b"b".to_vec(), 200);
        messenger.send_message(1, 2, [1u8; 32], b"c".to_vec(), 300);

        let expired = messenger.expire_messages(250);
        assert_eq!(expired, 2);
        assert_eq!(messenger.pending_for_destination(2).len(), 1);
    }
}
