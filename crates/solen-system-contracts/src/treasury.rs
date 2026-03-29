//! Treasury: fee collection, grant disbursement, and spending proposals.

use serde::{Deserialize, Serialize};
use solen_types::AccountId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TreasuryError {
    #[error("insufficient treasury balance: have {have}, need {need}")]
    InsufficientBalance { have: u128, need: u128 },
    #[error("unauthorized")]
    Unauthorized,
    #[error("grant not found")]
    GrantNotFound,
}

/// A grant disbursement from the treasury.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub id: u64,
    pub recipient: AccountId,
    pub amount: u128,
    pub description: String,
    pub disbursed: bool,
}

/// The treasury contract state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TreasuryContract {
    pub balance: u128,
    pub total_fees_collected: u128,
    pub total_burned: u128,
    pub total_grants_disbursed: u128,
    pub grants: Vec<Grant>,
    next_grant_id: u64,
}

impl TreasuryContract {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record fee collection.
    pub fn collect_fee(&mut self, amount: u128, burn_amount: u128) {
        let treasury_share = amount.saturating_sub(burn_amount);
        self.balance += treasury_share;
        self.total_fees_collected += amount;
        self.total_burned += burn_amount;
    }

    /// Create a grant (must be approved through governance).
    pub fn create_grant(
        &mut self,
        recipient: AccountId,
        amount: u128,
        description: String,
    ) -> Result<u64, TreasuryError> {
        if amount > self.balance {
            return Err(TreasuryError::InsufficientBalance {
                have: self.balance,
                need: amount,
            });
        }

        let id = self.next_grant_id;
        self.next_grant_id += 1;

        self.grants.push(Grant {
            id,
            recipient,
            amount,
            description,
            disbursed: false,
        });

        Ok(id)
    }

    /// Disburse a grant.
    pub fn disburse_grant(&mut self, grant_id: u64) -> Result<(AccountId, u128), TreasuryError> {
        let grant = self
            .grants
            .iter_mut()
            .find(|g| g.id == grant_id && !g.disbursed)
            .ok_or(TreasuryError::GrantNotFound)?;

        if grant.amount > self.balance {
            return Err(TreasuryError::InsufficientBalance {
                have: self.balance,
                need: grant.amount,
            });
        }

        self.balance -= grant.amount;
        self.total_grants_disbursed += grant.amount;
        grant.disbursed = true;

        Ok((grant.recipient, grant.amount))
    }

    const STORAGE_KEY: &'static [u8] = b"__treasury_state__";

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

    fn aid(n: u8) -> [u8; 32] {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn collect_and_disburse() {
        let mut treasury = TreasuryContract::new();

        treasury.collect_fee(1000, 500); // 500 burned, 500 to treasury
        assert_eq!(treasury.balance, 500);
        assert_eq!(treasury.total_burned, 500);

        let gid = treasury
            .create_grant(aid(1), 200, "dev grant".into())
            .unwrap();
        let (recipient, amount) = treasury.disburse_grant(gid).unwrap();
        assert_eq!(recipient, aid(1));
        assert_eq!(amount, 200);
        assert_eq!(treasury.balance, 300);
    }

    #[test]
    fn insufficient_grant() {
        let mut treasury = TreasuryContract::new();
        treasury.collect_fee(100, 0);

        let err = treasury
            .create_grant(aid(1), 200, "too much".into())
            .unwrap_err();
        assert!(matches!(err, TreasuryError::InsufficientBalance { .. }));
    }
}
