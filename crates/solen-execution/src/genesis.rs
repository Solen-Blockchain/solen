//! Genesis state initialization.

use solen_storage::StateStore;
use solen_types::account::AuthMethod;
use solen_types::AccountId;

use crate::state::{StateError, StateManager};

/// A genesis account allocation.
pub struct GenesisAccount {
    pub id: AccountId,
    pub balance: u128,
    pub auth_methods: Vec<AuthMethod>,
}

/// Initialize the state store with genesis accounts.
pub fn apply_genesis(
    store: &mut dyn StateStore,
    accounts: Vec<GenesisAccount>,
) -> Result<(), StateError> {
    let mut state = StateManager::new(store);
    for ga in accounts {
        state.create_account(ga.id, ga.auth_methods, ga.balance)?;
    }
    Ok(())
}
