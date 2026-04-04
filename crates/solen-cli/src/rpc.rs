//! JSON-RPC client for the CLI.

use anyhow::{anyhow, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[derive(Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'a str,
    method: &'a str,
    params: serde_json::Value,
    id: u64,
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

pub struct RpcClient {
    url: String,
    client: reqwest::Client,
}

impl RpcClient {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            client: reqwest::Client::new(),
        }
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T> {
        let req = RpcRequest {
            jsonrpc: "2.0",
            method,
            params,
            id: 1,
        };

        let resp = self
            .client
            .post(&self.url)
            .json(&req)
            .send()
            .await
            .map_err(|e| anyhow!("connection failed: {e}\nIs the node running at {}?", self.url))?;

        let body: RpcResponse<T> = resp.json().await?;

        if let Some(err) = body.error {
            return Err(anyhow!("RPC error {}: {}", err.code, err.message));
        }

        body.result.ok_or_else(|| anyhow!("empty RPC response"))
    }

    pub async fn chain_status(&self) -> Result<ChainStatus> {
        self.call("solen_chainStatus", serde_json::json!([])).await
    }

    pub async fn get_balance(&self, account_id: &str) -> Result<String> {
        self.call("solen_getBalance", serde_json::json!([account_id]))
            .await
    }

    pub async fn get_account(&self, account_id: &str) -> Result<AccountInfo> {
        self.call("solen_getAccount", serde_json::json!([account_id]))
            .await
    }

    pub async fn get_block(&self, height: u64) -> Result<BlockInfo> {
        self.call("solen_getBlock", serde_json::json!([height]))
            .await
    }

    pub async fn get_latest_block(&self) -> Result<BlockInfo> {
        self.call("solen_getLatestBlock", serde_json::json!([])).await
    }

    pub async fn submit_operation(&self, op: serde_json::Value) -> Result<SubmitResult> {
        self.call("solen_submitOperation", serde_json::json!([op]))
            .await
    }

    pub async fn simulate_operation(&self, op: serde_json::Value) -> Result<SimulationResult> {
        self.call("solen_simulateOperation", serde_json::json!([op]))
            .await
    }

    pub async fn get_validators(&self) -> Result<Vec<ValidatorInfo>> {
        self.call("solen_getValidators", serde_json::json!([])).await
    }
}

#[derive(Debug, Deserialize)]
pub struct ChainStatus {
    pub height: u64,
    pub state_root: String,
    pub pending_ops: u64,
}

#[derive(Debug, Deserialize)]
pub struct AccountInfo {
    pub id: String,
    pub balance: String,
    pub nonce: u64,
    pub code_hash: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct BlockInfo {
    pub height: u64,
    pub epoch: u64,
    pub parent_hash: String,
    pub state_root: String,
    pub proposer: String,
    pub timestamp_ms: u64,
    pub tx_count: usize,
    pub gas_used: u64,
}

#[derive(Debug, Deserialize)]
pub struct SubmitResult {
    pub accepted: bool,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SimulationResult {
    pub success: bool,
    pub gas_used: u64,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ValidatorInfo {
    pub address: String,
    pub self_stake: String,
    pub total_delegated: String,
    pub total_stake: String,
    pub is_active: bool,
    pub is_genesis: bool,
}
