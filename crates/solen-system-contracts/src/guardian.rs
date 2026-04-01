//! Guardian recovery contract: social recovery for lost keys.
//!
//! Users designate trusted guardians on their account. If they lose
//! access, guardians can collectively initiate a recovery to replace
//! the account's auth methods. A 1-week timelock gives the real owner
//! time to cancel unauthorized recovery attempts.

use serde::{Deserialize, Serialize};
use solen_types::account::AuthMethod;
use solen_types::AccountId;

/// Recovery timelock: 1 week in blocks.
/// At 4s block time: 7 * 24 * 3600 / 4 = 151,200 blocks.
pub const RECOVERY_TIMELOCK_BLOCKS: u64 = 151_200;

/// Minimum number of guardian confirmations required.
/// At least 2 guardians must confirm, or all guardians if fewer than 2.
pub const MIN_CONFIRMATIONS: usize = 2;

/// Status of a recovery request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryStatus {
    Pending,
    Cancelled,
    Executed,
}

/// A pending recovery request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryRequest {
    pub id: u64,
    /// The account being recovered.
    pub target_account: AccountId,
    /// The new auth methods to set if recovery succeeds.
    pub new_auth_methods: Vec<AuthMethod>,
    /// Block height when the recovery was initiated.
    pub initiated_at: u64,
    /// Block height after which recovery can be executed.
    pub execute_after: u64,
    /// Guardians who have confirmed this recovery.
    pub confirmations: Vec<AccountId>,
    /// Guardian IDs captured at initiation time. Only these guardians
    /// can confirm — prevents attacks where guardians change between
    /// initiation and confirmation.
    pub guardian_ids: Vec<AccountId>,
    /// Minimum confirmations needed (majority of guardians, min 2).
    pub threshold: usize,
    pub status: RecoveryStatus,
}

/// The guardian recovery contract state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GuardianContract {
    pub recovery_requests: Vec<RecoveryRequest>,
    next_id: u64,
}

impl GuardianContract {
    pub fn new() -> Self {
        Self::default()
    }

    /// Initiate a recovery for a target account.
    /// The sender must be a guardian of the target account.
    pub fn initiate_recovery(
        &mut self,
        target_account: AccountId,
        initiator: AccountId,
        new_auth_methods: Vec<AuthMethod>,
        guardian_ids: &[AccountId],
        current_height: u64,
    ) -> Result<u64, String> {
        // Reject empty guardian list (would allow 0-threshold recovery).
        if guardian_ids.is_empty() {
            return Err("target account has no guardians".into());
        }

        // Verify the initiator is a guardian.
        if !guardian_ids.contains(&initiator) {
            return Err("sender is not a guardian of the target account".into());
        }

        // Check no active recovery already exists for this account.
        if self.recovery_requests.iter().any(|r| {
            r.target_account == target_account && r.status == RecoveryStatus::Pending
        }) {
            return Err("active recovery already exists for this account".into());
        }

        if new_auth_methods.is_empty() {
            return Err("new_auth_methods cannot be empty".into());
        }

        // Threshold = majority of guardians, minimum 2 (or all if < 2).
        let threshold = if guardian_ids.len() < MIN_CONFIRMATIONS {
            guardian_ids.len()
        } else {
            (guardian_ids.len() / 2) + 1
        };

        let id = self.next_id;
        self.next_id += 1;

        self.recovery_requests.push(RecoveryRequest {
            id,
            target_account,
            new_auth_methods,
            initiated_at: current_height,
            execute_after: current_height.saturating_add(RECOVERY_TIMELOCK_BLOCKS),
            confirmations: vec![initiator], // initiator auto-confirms
            guardian_ids: guardian_ids.to_vec(), // snapshot at initiation time
            threshold,
            status: RecoveryStatus::Pending,
        });

        Ok(id)
    }

    /// Confirm a recovery request. Sender must be one of the guardians
    /// captured at initiation time (not the current account guardians).
    pub fn confirm_recovery(
        &mut self,
        recovery_id: u64,
        confirmer: AccountId,
    ) -> Result<(), String> {
        let req = self.recovery_requests.iter_mut()
            .find(|r| r.id == recovery_id && r.status == RecoveryStatus::Pending)
            .ok_or("recovery request not found or not pending")?;

        // Validate against the guardian list captured at initiation.
        if !req.guardian_ids.contains(&confirmer) {
            return Err("sender is not a guardian of the target account".into());
        }

        if req.confirmations.contains(&confirmer) {
            return Err("already confirmed".into());
        }

        req.confirmations.push(confirmer);
        Ok(())
    }

    /// Cancel a recovery request. Only the target account owner can cancel.
    pub fn cancel_recovery(
        &mut self,
        recovery_id: u64,
        sender: &AccountId,
    ) -> Result<(), String> {
        let req = self.recovery_requests.iter_mut()
            .find(|r| r.id == recovery_id && r.status == RecoveryStatus::Pending)
            .ok_or("recovery request not found or not pending")?;

        if req.target_account != *sender {
            return Err("only the account owner can cancel recovery".into());
        }

        req.status = RecoveryStatus::Cancelled;
        Ok(())
    }

    /// Check if a recovery is ready to execute.
    pub fn can_execute(
        &self,
        recovery_id: u64,
        current_height: u64,
    ) -> Result<&RecoveryRequest, String> {
        let req = self.recovery_requests.iter()
            .find(|r| r.id == recovery_id && r.status == RecoveryStatus::Pending)
            .ok_or("recovery request not found or not pending")?;

        if current_height < req.execute_after {
            return Err(format!(
                "timelock not expired: {} blocks remaining",
                req.execute_after - current_height
            ));
        }

        if req.confirmations.len() < req.threshold {
            return Err(format!(
                "insufficient confirmations: {} of {} required",
                req.confirmations.len(), req.threshold
            ));
        }

        Ok(req)
    }

    /// Mark a recovery as executed. The caller is responsible for
    /// actually updating the account's auth methods.
    pub fn mark_executed(&mut self, recovery_id: u64) -> Result<RecoveryRequest, String> {
        let req = self.recovery_requests.iter_mut()
            .find(|r| r.id == recovery_id && r.status == RecoveryStatus::Pending)
            .ok_or("recovery request not found or not pending")?;

        req.status = RecoveryStatus::Executed;
        Ok(req.clone())
    }

    /// Get active recovery for an account (if any).
    pub fn active_recovery(&self, account: &AccountId) -> Option<&RecoveryRequest> {
        self.recovery_requests.iter().find(|r| {
            r.target_account == *account && r.status == RecoveryStatus::Pending
        })
    }

    const STORAGE_KEY: &'static [u8] = b"__guardian_state__";

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

    fn ed25519_auth(n: u8) -> AuthMethod {
        let mut pk = [0u8; 32];
        pk[0] = n;
        AuthMethod::Ed25519 { public_key: pk }
    }

    #[test]
    fn initiate_and_execute_recovery() {
        let mut contract = GuardianContract::new();
        let target = aid(1);
        let guardians = vec![aid(10), aid(11), aid(12)];
        let new_auth = vec![ed25519_auth(99)];

        // Guardian 10 initiates recovery.
        let id = contract.initiate_recovery(
            target, aid(10), new_auth.clone(), &guardians, 1000,
        ).unwrap();

        // Threshold is majority: 2 of 3.
        let req = &contract.recovery_requests[0];
        assert_eq!(req.threshold, 2);
        assert_eq!(req.confirmations.len(), 1); // initiator auto-confirmed

        // Guardian 11 confirms.
        contract.confirm_recovery(id, aid(11)).unwrap();
        assert_eq!(contract.recovery_requests[0].confirmations.len(), 2);

        // Can't execute yet — timelock.
        assert!(contract.can_execute(id, 1000).is_err());

        // After timelock.
        let req = contract.can_execute(id, 1000 + RECOVERY_TIMELOCK_BLOCKS).unwrap();
        assert_eq!(req.new_auth_methods.len(), 1);

        // Execute.
        let executed = contract.mark_executed(id).unwrap();
        assert_eq!(executed.status, RecoveryStatus::Executed);
    }

    #[test]
    fn owner_can_cancel() {
        let mut contract = GuardianContract::new();
        let target = aid(1);
        let guardians = vec![aid(10), aid(11)];

        let id = contract.initiate_recovery(
            target, aid(10), vec![ed25519_auth(99)], &guardians, 100,
        ).unwrap();

        // Non-owner can't cancel.
        assert!(contract.cancel_recovery(id, &aid(10)).is_err());

        // Owner cancels.
        contract.cancel_recovery(id, &aid(1)).unwrap();
        assert_eq!(contract.recovery_requests[0].status, RecoveryStatus::Cancelled);
    }

    #[test]
    fn non_guardian_rejected() {
        let mut contract = GuardianContract::new();
        let guardians = vec![aid(10), aid(11)];

        let result = contract.initiate_recovery(
            aid(1), aid(99), vec![ed25519_auth(1)], &guardians, 100,
        );
        assert!(result.is_err());
    }

    #[test]
    fn duplicate_recovery_rejected() {
        let mut contract = GuardianContract::new();
        let guardians = vec![aid(10), aid(11)];

        contract.initiate_recovery(
            aid(1), aid(10), vec![ed25519_auth(99)], &guardians, 100,
        ).unwrap();

        // Second recovery for same account rejected.
        let result = contract.initiate_recovery(
            aid(1), aid(11), vec![ed25519_auth(88)], &guardians, 200,
        );
        assert!(result.is_err());
    }

    #[test]
    fn insufficient_confirmations() {
        let mut contract = GuardianContract::new();
        let guardians = vec![aid(10), aid(11), aid(12)];

        let id = contract.initiate_recovery(
            aid(1), aid(10), vec![ed25519_auth(99)], &guardians, 100,
        ).unwrap();

        // Only 1 confirmation (initiator), need 2.
        let result = contract.can_execute(id, 100 + RECOVERY_TIMELOCK_BLOCKS);
        assert!(result.is_err());
    }
}
