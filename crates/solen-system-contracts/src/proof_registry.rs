//! Proof verifier registry: manages approved proof systems for rollup domains.

use serde::{Deserialize, Serialize};
use solen_types::RollupId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("proof type already registered: {0}")]
    AlreadyRegistered(String),
    #[error("proof type not found: {0}")]
    NotFound(String),
}

/// A registered proof system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofSystemInfo {
    pub name: String,
    pub description: String,
    pub is_active: bool,
    pub rollups_using: Vec<RollupId>,
}

/// Registry of approved proof systems.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProofRegistry {
    pub systems: Vec<ProofSystemInfo>,
}

impl ProofRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new proof system.
    pub fn register(
        &mut self,
        name: String,
        description: String,
    ) -> Result<(), RegistryError> {
        if self.systems.iter().any(|s| s.name == name) {
            return Err(RegistryError::AlreadyRegistered(name));
        }
        self.systems.push(ProofSystemInfo {
            name,
            description,
            is_active: true,
            rollups_using: Vec::new(),
        });
        Ok(())
    }

    /// Associate a rollup with a proof system.
    pub fn add_rollup(
        &mut self,
        proof_type: &str,
        rollup_id: RollupId,
    ) -> Result<(), RegistryError> {
        let system = self
            .systems
            .iter_mut()
            .find(|s| s.name == proof_type)
            .ok_or_else(|| RegistryError::NotFound(proof_type.to_string()))?;
        if !system.rollups_using.contains(&rollup_id) {
            system.rollups_using.push(rollup_id);
        }
        Ok(())
    }

    /// Get all active proof system names.
    pub fn active_systems(&self) -> Vec<&str> {
        self.systems
            .iter()
            .filter(|s| s.is_active)
            .map(|s| s.name.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_query() {
        let mut registry = ProofRegistry::new();
        registry
            .register("validity-zk".into(), "ZK validity proofs".into())
            .unwrap();
        registry
            .register("fraud-interactive".into(), "Interactive fraud proofs".into())
            .unwrap();

        assert_eq!(registry.active_systems().len(), 2);

        registry.add_rollup("validity-zk", 1).unwrap();
        assert_eq!(registry.systems[0].rollups_using, vec![1]);
    }

    #[test]
    fn duplicate_rejected() {
        let mut registry = ProofRegistry::new();
        registry.register("mock".into(), "Mock".into()).unwrap();
        assert!(registry.register("mock".into(), "Mock2".into()).is_err());
    }
}
