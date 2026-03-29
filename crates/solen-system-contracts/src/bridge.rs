//! Canonical bridge contract: deposits, withdrawals, vault accounting.
//!
//! Each rollup has a bridge vault that holds deposited assets. Withdrawals
//! go through a challenge window before finalization.

use serde::{Deserialize, Serialize};
use solen_types::{AccountId, Hash, RollupId};
use thiserror::Error;

/// Default challenge window in blocks.
pub const DEFAULT_CHALLENGE_WINDOW: u64 = 100;

/// Default withdrawal delay in blocks.
pub const DEFAULT_WITHDRAWAL_DELAY: u64 = 50;

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("vault not found for rollup {0}")]
    VaultNotFound(RollupId),
    #[error("insufficient vault balance: have {have}, need {need}")]
    InsufficientVaultBalance { have: u128, need: u128 },
    #[error("withdrawal not found")]
    WithdrawalNotFound,
    #[error("withdrawal not ready: {remaining} blocks remaining")]
    WithdrawalNotReady { remaining: u64 },
    #[error("withdrawal disputed")]
    WithdrawalDisputed,
    #[error("vault already exists for rollup {0}")]
    VaultAlreadyExists(RollupId),
}

/// Status of a pending withdrawal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WithdrawalStatus {
    Pending,
    Disputed,
    Finalized,
}

/// A pending withdrawal from L2 to L1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingWithdrawal {
    pub id: u64,
    pub rollup_id: RollupId,
    pub recipient: AccountId,
    pub amount: u128,
    pub initiated_block: u64,
    pub finalize_after_block: u64,
    pub proof_hash: Hash,
    pub status: WithdrawalStatus,
}

/// A bridge vault for a single rollup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeVault {
    pub rollup_id: RollupId,
    pub balance: u128,
    pub total_deposited: u128,
    pub total_withdrawn: u128,
    pub challenge_window: u64,
    pub withdrawal_delay: u64,
}

/// The bridge contract state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BridgeContract {
    pub vaults: Vec<BridgeVault>,
    pub pending_withdrawals: Vec<PendingWithdrawal>,
    next_withdrawal_id: u64,
}

impl BridgeContract {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new bridge vault for a rollup.
    pub fn register_vault(&mut self, rollup_id: RollupId) -> Result<(), BridgeError> {
        if self.vaults.iter().any(|v| v.rollup_id == rollup_id) {
            return Err(BridgeError::VaultAlreadyExists(rollup_id));
        }
        self.vaults.push(BridgeVault {
            rollup_id,
            balance: 0,
            total_deposited: 0,
            total_withdrawn: 0,
            challenge_window: DEFAULT_CHALLENGE_WINDOW,
            withdrawal_delay: DEFAULT_WITHDRAWAL_DELAY,
        });
        Ok(())
    }

    /// Deposit assets into a rollup's bridge vault.
    pub fn deposit(
        &mut self,
        rollup_id: RollupId,
        amount: u128,
    ) -> Result<(), BridgeError> {
        let vault = self
            .vaults
            .iter_mut()
            .find(|v| v.rollup_id == rollup_id)
            .ok_or(BridgeError::VaultNotFound(rollup_id))?;

        vault.balance += amount;
        vault.total_deposited += amount;
        Ok(())
    }

    /// Initiate a withdrawal from a rollup. Subject to challenge window.
    pub fn initiate_withdrawal(
        &mut self,
        rollup_id: RollupId,
        recipient: AccountId,
        amount: u128,
        current_block: u64,
        proof_hash: Hash,
    ) -> Result<u64, BridgeError> {
        let vault = self
            .vaults
            .iter()
            .find(|v| v.rollup_id == rollup_id)
            .ok_or(BridgeError::VaultNotFound(rollup_id))?;

        if vault.balance < amount {
            return Err(BridgeError::InsufficientVaultBalance {
                have: vault.balance,
                need: amount,
            });
        }

        let id = self.next_withdrawal_id;
        self.next_withdrawal_id += 1;

        let finalize_after = current_block + vault.challenge_window + vault.withdrawal_delay;

        self.pending_withdrawals.push(PendingWithdrawal {
            id,
            rollup_id,
            recipient,
            amount,
            initiated_block: current_block,
            finalize_after_block: finalize_after,
            proof_hash,
            status: WithdrawalStatus::Pending,
        });

        Ok(id)
    }

    /// Dispute a pending withdrawal (e.g., fraud proof submitted).
    pub fn dispute_withdrawal(&mut self, withdrawal_id: u64) -> Result<(), BridgeError> {
        let w = self
            .pending_withdrawals
            .iter_mut()
            .find(|w| w.id == withdrawal_id)
            .ok_or(BridgeError::WithdrawalNotFound)?;

        if w.status != WithdrawalStatus::Pending {
            return Err(BridgeError::WithdrawalNotFound);
        }

        w.status = WithdrawalStatus::Disputed;
        Ok(())
    }

    /// Finalize a withdrawal after the challenge period.
    /// Returns the amount withdrawn if successful.
    pub fn finalize_withdrawal(
        &mut self,
        withdrawal_id: u64,
        current_block: u64,
    ) -> Result<(AccountId, u128), BridgeError> {
        let w = self
            .pending_withdrawals
            .iter_mut()
            .find(|w| w.id == withdrawal_id)
            .ok_or(BridgeError::WithdrawalNotFound)?;

        match w.status {
            WithdrawalStatus::Disputed => return Err(BridgeError::WithdrawalDisputed),
            WithdrawalStatus::Finalized => return Err(BridgeError::WithdrawalNotFound),
            WithdrawalStatus::Pending => {}
        }

        if current_block < w.finalize_after_block {
            return Err(BridgeError::WithdrawalNotReady {
                remaining: w.finalize_after_block - current_block,
            });
        }

        let recipient = w.recipient;
        let amount = w.amount;
        let rollup_id = w.rollup_id;
        w.status = WithdrawalStatus::Finalized;

        // Deduct from vault.
        let vault = self
            .vaults
            .iter_mut()
            .find(|v| v.rollup_id == rollup_id)
            .ok_or(BridgeError::VaultNotFound(rollup_id))?;

        vault.balance = vault.balance.saturating_sub(amount);
        vault.total_withdrawn += amount;

        Ok((recipient, amount))
    }

    /// Get vault info for a rollup.
    pub fn get_vault(&self, rollup_id: RollupId) -> Option<&BridgeVault> {
        self.vaults.iter().find(|v| v.rollup_id == rollup_id)
    }

    /// Get all pending withdrawals for a recipient.
    pub fn pending_for_recipient(&self, recipient: &AccountId) -> Vec<&PendingWithdrawal> {
        self.pending_withdrawals
            .iter()
            .filter(|w| w.recipient == *recipient && w.status == WithdrawalStatus::Pending)
            .collect()
    }

    const STORAGE_KEY: &'static [u8] = b"__bridge_state__";

    pub fn load(store: &dyn solen_storage::StateStore) -> Self {
        match store.get(Self::STORAGE_KEY) {
            Ok(Some(data)) => serde_json::from_slice(&data).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    pub fn save(&self, store: &mut dyn solen_storage::StateStore) {
        if let Ok(data) = serde_json::to_vec(self) {
            let _ = store.put(Self::STORAGE_KEY, &data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aid(n: u8) -> AccountId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn deposit_and_withdraw_lifecycle() {
        let mut bridge = BridgeContract::new();

        bridge.register_vault(1).unwrap();
        bridge.deposit(1, 10_000).unwrap();

        assert_eq!(bridge.get_vault(1).unwrap().balance, 10_000);

        // Initiate withdrawal at block 100.
        let wid = bridge
            .initiate_withdrawal(1, aid(1), 3_000, 100, [0; 32])
            .unwrap();

        // Too early to finalize.
        let err = bridge.finalize_withdrawal(wid, 200).unwrap_err();
        assert!(matches!(err, BridgeError::WithdrawalNotReady { .. }));

        // After challenge window + delay (100 + 100 + 50 = 250).
        let (recipient, amount) = bridge.finalize_withdrawal(wid, 250).unwrap();
        assert_eq!(recipient, aid(1));
        assert_eq!(amount, 3_000);

        let vault = bridge.get_vault(1).unwrap();
        assert_eq!(vault.balance, 7_000);
        assert_eq!(vault.total_withdrawn, 3_000);
    }

    #[test]
    fn dispute_blocks_withdrawal() {
        let mut bridge = BridgeContract::new();
        bridge.register_vault(1).unwrap();
        bridge.deposit(1, 5_000).unwrap();

        let wid = bridge
            .initiate_withdrawal(1, aid(1), 2_000, 10, [0; 32])
            .unwrap();

        bridge.dispute_withdrawal(wid).unwrap();

        let err = bridge.finalize_withdrawal(wid, 999).unwrap_err();
        assert!(matches!(err, BridgeError::WithdrawalDisputed));

        // Vault balance unchanged.
        assert_eq!(bridge.get_vault(1).unwrap().balance, 5_000);
    }

    #[test]
    fn insufficient_vault_balance() {
        let mut bridge = BridgeContract::new();
        bridge.register_vault(1).unwrap();
        bridge.deposit(1, 100).unwrap();

        let err = bridge
            .initiate_withdrawal(1, aid(1), 500, 1, [0; 32])
            .unwrap_err();
        assert!(matches!(err, BridgeError::InsufficientVaultBalance { .. }));
    }

    #[test]
    fn duplicate_vault_rejected() {
        let mut bridge = BridgeContract::new();
        bridge.register_vault(1).unwrap();
        let err = bridge.register_vault(1).unwrap_err();
        assert!(matches!(err, BridgeError::VaultAlreadyExists(1)));
    }

    #[test]
    fn multiple_withdrawals() {
        let mut bridge = BridgeContract::new();
        bridge.register_vault(1).unwrap();
        bridge.deposit(1, 10_000).unwrap();

        let w1 = bridge.initiate_withdrawal(1, aid(1), 1000, 10, [0; 32]).unwrap();
        let w2 = bridge.initiate_withdrawal(1, aid(2), 2000, 10, [0; 32]).unwrap();

        assert_eq!(bridge.pending_for_recipient(&aid(1)).len(), 1);
        assert_eq!(bridge.pending_for_recipient(&aid(2)).len(), 1);

        // Finalize both.
        bridge.finalize_withdrawal(w1, 300).unwrap();
        bridge.finalize_withdrawal(w2, 300).unwrap();

        assert_eq!(bridge.get_vault(1).unwrap().balance, 7_000);
    }
}
