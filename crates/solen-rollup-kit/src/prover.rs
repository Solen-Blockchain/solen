//! Prover adapter: interface for pluggable proof systems (validity/fraud).

pub trait ProverBackend: Send + Sync {
    fn generate_proof(&self, state_diff: &[u8]) -> Vec<u8>;
    fn proof_type(&self) -> &str;
}
