//! State management: account lookups, balance changes, nonce tracking.
//!
//! Account state is serialized with borsh (binary, fast) and stored via
//! the `StateStore` trait. Keys are prefixed: `acc/<id>` for accounts,
//! `code/<hash>` for bytecode, `cs/<id>/<key>` for contract storage.

use borsh::BorshDeserialize;
use solen_storage::{StateStore, StorageError};
use solen_types::account::{Account, AuthMethod};
use solen_types::{AccountId, Hash};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("account not found: {0}")]
    AccountNotFound(String),
    #[error("insufficient balance: have {have}, need {need}")]
    InsufficientBalance { have: u128, need: u128 },
    #[error("invalid nonce: expected {expected}, got {got}")]
    InvalidNonce { expected: u64, got: u64 },
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("serialization error: {0}")]
    Serialization(String),
}

/// Key prefix for account data.
fn account_key(id: &AccountId) -> Vec<u8> {
    let mut key = b"acc/".to_vec();
    key.extend_from_slice(id);
    key
}

fn load_account(store: &dyn StateStore, id: &AccountId) -> Result<Option<Account>, StateError> {
    let key = account_key(id);
    match store.get(&key)? {
        Some(data) => {
            let account = Account::try_from_slice(&data)
                .map_err(|e| StateError::Serialization(e.to_string()))?;
            Ok(Some(account))
        }
        None => Ok(None),
    }
}

fn require_account(store: &dyn StateStore, id: &AccountId) -> Result<Account, StateError> {
    load_account(store, id)?
        .ok_or_else(|| StateError::AccountNotFound(hex::encode(id)))
}

fn save_account(store: &mut dyn StateStore, account: &Account) -> Result<(), StateError> {
    let key = account_key(&account.id);
    let data = borsh::to_vec(account).map_err(|e| StateError::Serialization(e.to_string()))?;
    store.put(&key, &data)?;
    Ok(())
}

// ---------- Read-only state manager (for RPC) ----------

/// Read-only state accessor for RPC queries and simulation.
pub struct ReadonlyStateManager<'a> {
    store: &'a dyn StateStore,
}

impl<'a> ReadonlyStateManager<'a> {
    pub fn new(store: &'a dyn StateStore) -> Self {
        Self { store }
    }

    pub fn get_account(&self, id: &AccountId) -> Result<Option<Account>, StateError> {
        load_account(self.store, id)
    }

    pub fn require_account(&self, id: &AccountId) -> Result<Account, StateError> {
        require_account(self.store, id)
    }

    pub fn get_balance(&self, id: &AccountId) -> Result<u128, StateError> {
        Ok(load_account(self.store, id)?
            .map(|a| a.balance)
            .unwrap_or(0))
    }

    pub fn state_root(&self) -> Hash {
        self.store.state_root()
    }
}

// ---------- Mutable state manager ----------

/// Manages account state backed by a `StateStore`.
pub struct StateManager<'a> {
    store: &'a mut dyn StateStore,
}

impl<'a> StateManager<'a> {
    pub fn new(store: &'a mut dyn StateStore) -> Self {
        Self { store }
    }

    /// Convenience constructor that returns a read-only manager.
    pub fn new_readonly(store: &'a dyn StateStore) -> ReadonlyStateManager<'a> {
        ReadonlyStateManager::new(store)
    }

    /// Create a new account with the given ID, auth methods, and initial balance.
    pub fn create_account(
        &mut self,
        id: AccountId,
        auth_methods: Vec<AuthMethod>,
        initial_balance: u128,
    ) -> Result<Account, StateError> {
        let account = Account {
            id,
            code_hash: [0u8; 32],
            auth_methods,
            nonce: 0,
            balance: initial_balance,
        };
        self.save_account(&account)?;
        Ok(account)
    }

    /// Load an account by ID. Returns `None` if it doesn't exist.
    pub fn get_account(&self, id: &AccountId) -> Result<Option<Account>, StateError> {
        load_account(self.store, id)
    }

    /// Load an account, returning an error if it doesn't exist.
    pub fn require_account(&self, id: &AccountId) -> Result<Account, StateError> {
        require_account(self.store, id)
    }

    /// Persist an account to the store.
    pub fn save_account(&mut self, account: &Account) -> Result<(), StateError> {
        save_account(self.store, account)
    }

    /// Get balance for an account. Returns 0 if the account doesn't exist.
    pub fn get_balance(&self, id: &AccountId) -> Result<u128, StateError> {
        Ok(self.get_account(id)?.map(|a| a.balance).unwrap_or(0))
    }

    /// Transfer `amount` from `from` to `to`. Both accounts must exist.
    pub fn transfer(
        &mut self,
        from: &AccountId,
        to: &AccountId,
        amount: u128,
    ) -> Result<(), StateError> {
        let mut sender = self.require_account(from)?;
        if sender.balance < amount {
            return Err(StateError::InsufficientBalance {
                have: sender.balance,
                need: amount,
            });
        }
        sender.balance -= amount;
        self.save_account(&sender)?;

        let mut receiver = match self.get_account(to)? {
            Some(acc) => acc,
            None => {
                // Don't auto-create system contract accounts.
                if solen_types::system::is_system_contract(to) {
                    return Err(StateError::AccountNotFound(
                        "cannot transfer to system contract".into(),
                    ));
                }
                // Auto-create recipient account on first transfer.
                // Since account ID = public key, use it as the auth method.
                let auth = vec![AuthMethod::Ed25519 { public_key: *to }];
                self.create_account(*to, auth, 0)?;
                self.require_account(to)?
            }
        };
        receiver.balance = receiver.balance.saturating_add(amount);
        self.save_account(&receiver)?;

        Ok(())
    }

    /// Validate and consume a nonce for the given account.
    pub fn consume_nonce(&mut self, id: &AccountId, nonce: u64) -> Result<(), StateError> {
        let mut account = self.require_account(id)?;
        if nonce != account.nonce {
            return Err(StateError::InvalidNonce {
                expected: account.nonce,
                got: nonce,
            });
        }
        account.nonce = account.nonce.checked_add(1).ok_or_else(|| {
            StateError::Serialization("nonce overflow".into())
        })?;
        self.save_account(&account)?;
        Ok(())
    }

    /// Returns the current state root from the underlying store.
    pub fn state_root(&self) -> Hash {
        self.store.state_root()
    }

    // ---------- Bytecode storage ----------

    /// Store contract bytecode keyed by its hash.
    pub fn store_bytecode(&mut self, code: &[u8]) -> Result<Hash, StateError> {
        let hash = solen_crypto::blake3_hash(code);
        let key = bytecode_key(&hash);
        self.store.put(&key, code)?;
        Ok(hash)
    }

    /// Load contract bytecode by its hash.
    pub fn load_bytecode(&self, code_hash: &Hash) -> Result<Option<Vec<u8>>, StateError> {
        let key = bytecode_key(code_hash);
        Ok(self.store.get(&key)?)
    }

    // ---------- Contract storage ----------

    /// Read a value from a contract's storage.
    pub fn contract_storage_get(
        &self,
        contract_id: &AccountId,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, StateError> {
        let store_key = contract_storage_key(contract_id, key);
        Ok(self.store.get(&store_key)?)
    }

    /// Write a value to a contract's storage.
    pub fn contract_storage_set(
        &mut self,
        contract_id: &AccountId,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), StateError> {
        let store_key = contract_storage_key(contract_id, key);
        self.store.put(&store_key, value)?;
        Ok(())
    }

    /// Load all contract storage into a HashMap (for VM execution).
    pub fn load_contract_storage(
        &self,
        contract_id: &AccountId,
    ) -> Result<std::collections::HashMap<Vec<u8>, Vec<u8>>, StateError> {
        let prefix = contract_storage_prefix(contract_id);
        let mut map = std::collections::HashMap::new();
        // Scan all keys with the contract prefix.
        // For MemoryStore this works via iteration; for production
        // we'd use a prefix scan. For now, we use a simple approach
        // by loading known keys from the contract's storage manifest.
        // TODO: implement proper prefix iteration on StateStore trait.
        //
        // For now, contracts track their own keys via a manifest key.
        let manifest_key = contract_storage_key(contract_id, b"__keys__");
        if let Some(manifest_data) = self.store.get(&manifest_key)? {
            let keys: Vec<Vec<u8>> = serde_json::from_slice(&manifest_data)
                .unwrap_or_default();
            for key in keys {
                let store_key = contract_storage_key(contract_id, &key);
                if let Some(val) = self.store.get(&store_key)? {
                    map.insert(key, val);
                }
            }
        }
        let _ = prefix; // used for future prefix scan
        Ok(map)
    }

    /// Persist contract storage from a HashMap back to the store.
    pub fn save_contract_storage(
        &mut self,
        contract_id: &AccountId,
        storage: &std::collections::HashMap<Vec<u8>, Vec<u8>>,
    ) -> Result<(), StateError> {
        // Save each key-value pair.
        let keys: Vec<Vec<u8>> = storage.keys().cloned().collect();
        for (key, value) in storage {
            let store_key = contract_storage_key(contract_id, key);
            self.store.put(&store_key, value)?;
        }
        // Save the key manifest so we can reload later.
        let manifest_key = contract_storage_key(contract_id, b"__keys__");
        let manifest_data = serde_json::to_vec(&keys)
            .map_err(|e| StateError::Serialization(e.to_string()))?;
        self.store.put(&manifest_key, &manifest_data)?;
        Ok(())
    }
}

/// Build a storage key for contract bytecode: "code/{code_hash}"
fn bytecode_key(code_hash: &Hash) -> Vec<u8> {
    let mut k = b"code/".to_vec();
    k.extend_from_slice(code_hash);
    k
}

/// Build a storage key for contract-specific data: "cs/{contract_id}/{key}"
fn contract_storage_key(contract_id: &AccountId, key: &[u8]) -> Vec<u8> {
    let mut k = b"cs/".to_vec();
    k.extend_from_slice(contract_id);
    k.push(b'/');
    k.extend_from_slice(key);
    k
}

fn contract_storage_prefix(contract_id: &AccountId) -> Vec<u8> {
    let mut k = b"cs/".to_vec();
    k.extend_from_slice(contract_id);
    k.push(b'/');
    k
}

/// Minimal hex encoding for error messages (avoids adding a dependency).
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solen_storage::MemoryStore;

    fn test_account_id(n: u8) -> AccountId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn create_and_get_account() {
        let mut store = MemoryStore::new();
        let mut state = StateManager::new(&mut store);

        let id = test_account_id(1);
        let auth = vec![AuthMethod::Ed25519 {
            public_key: [1u8; 32],
        }];
        let account = state.create_account(id, auth, 1000).unwrap();

        assert_eq!(account.balance, 1000);
        assert_eq!(account.nonce, 0);

        let loaded = state.get_account(&id).unwrap().unwrap();
        assert_eq!(loaded.balance, 1000);
    }

    #[test]
    fn transfer_works() {
        let mut store = MemoryStore::new();
        let mut state = StateManager::new(&mut store);

        let alice = test_account_id(1);
        let bob = test_account_id(2);
        state.create_account(alice, vec![], 500).unwrap();
        state.create_account(bob, vec![], 100).unwrap();

        state.transfer(&alice, &bob, 200).unwrap();

        assert_eq!(state.get_balance(&alice).unwrap(), 300);
        assert_eq!(state.get_balance(&bob).unwrap(), 300);
    }

    #[test]
    fn transfer_insufficient_balance() {
        let mut store = MemoryStore::new();
        let mut state = StateManager::new(&mut store);

        let alice = test_account_id(1);
        let bob = test_account_id(2);
        state.create_account(alice, vec![], 50).unwrap();
        state.create_account(bob, vec![], 0).unwrap();

        let err = state.transfer(&alice, &bob, 100).unwrap_err();
        assert!(matches!(err, StateError::InsufficientBalance { .. }));
    }

    #[test]
    fn nonce_management() {
        let mut store = MemoryStore::new();
        let mut state = StateManager::new(&mut store);

        let id = test_account_id(1);
        state.create_account(id, vec![], 0).unwrap();

        state.consume_nonce(&id, 0).unwrap();
        state.consume_nonce(&id, 1).unwrap();

        let err = state.consume_nonce(&id, 5).unwrap_err();
        assert!(matches!(err, StateError::InvalidNonce { expected: 2, got: 5 }));
    }

    #[test]
    fn state_root_changes_after_mutations() {
        let mut store = MemoryStore::new();
        let root_empty = store.state_root();

        let mut state = StateManager::new(&mut store);
        state.create_account(test_account_id(1), vec![], 100).unwrap();
        let root_after = state.state_root();

        assert_ne!(root_empty, root_after);
    }

    #[test]
    fn readonly_manager() {
        let mut store = MemoryStore::new();
        {
            let mut state = StateManager::new(&mut store);
            state.create_account(test_account_id(1), vec![], 777).unwrap();
        }

        let ro = ReadonlyStateManager::new(&store);
        assert_eq!(ro.get_balance(&test_account_id(1)).unwrap(), 777);
        assert_eq!(ro.get_balance(&test_account_id(99)).unwrap(), 0);
    }
}
