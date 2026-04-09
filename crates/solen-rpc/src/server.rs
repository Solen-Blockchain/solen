//! RPC server setup and lifecycle.

use std::net::SocketAddr;
use std::sync::Arc;

use jsonrpsee::server::Server;
use solen_consensus::engine::ConsensusEngine;
use thiserror::Error;
use tracing::info;

use crate::methods::{SolenApiServer, SolenRpc};

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("server error: {0}")]
    Server(String),
}

/// Start the JSON-RPC server on the given address.
pub async fn start_rpc_server(
    addr: SocketAddr,
    engine: Arc<ConsensusEngine>,
) -> Result<jsonrpsee::server::ServerHandle, RpcError> {
    // Per-IP rate limiting should be configured at the reverse proxy layer
    // (nginx/caddy) in production. The RPC server provides global rate limits
    // via RpcRateLimiter on write operations (submit_operation, submit_solution).
    let server = Server::builder()
        .max_connections(100)
        .max_request_body_size(1024 * 1024) // 1 MB max request
        .max_response_body_size(100 * 1024 * 1024) // 100 MB max response
        .build(addr)
        .await
        .map_err(|e| RpcError::Server(e.to_string()))?;

    let rpc = SolenRpc::new(engine);
    let handle = server.start(rpc.into_rpc());

    info!(%addr, "JSON-RPC server started");

    Ok(handle)
}
