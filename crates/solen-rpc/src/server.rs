//! RPC server setup and lifecycle.

use std::net::SocketAddr;
use std::sync::Arc;

use jsonrpsee::server::Server;
use tower_http::cors::{CorsLayer, Any};
use solen_consensus::engine::ConsensusEngine;
use thiserror::Error;
use tracing::info;

use crate::methods::{SolenApiServer, SolenRpc, TxBroadcaster};

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
    start_rpc_server_with_broadcast(addr, engine, None).await
}

/// Start the JSON-RPC server with optional P2P broadcast for submitted transactions.
pub async fn start_rpc_server_with_broadcast(
    addr: SocketAddr,
    engine: Arc<ConsensusEngine>,
    broadcaster: Option<TxBroadcaster>,
) -> Result<jsonrpsee::server::ServerHandle, RpcError> {
    // Per-IP rate limiting should be configured at the reverse proxy layer
    // (nginx/caddy) in production. The RPC server provides global rate limits
    // via RpcRateLimiter on write operations (submit_operation, submit_solution).
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let server = Server::builder()
        .set_http_middleware(tower::ServiceBuilder::new().layer(cors))
        .max_connections(100)
        .max_request_body_size(10 * 1024 * 1024) // 10 MB max request (supports ~4MB contract deploys)
        .max_response_body_size(100 * 1024 * 1024) // 100 MB max response
        .build(addr)
        .await
        .map_err(|e| RpcError::Server(e.to_string()))?;

    let rpc = SolenRpc::new(engine, broadcaster);
    let handle = server.start(rpc.into_rpc());

    info!(%addr, "JSON-RPC server started (HTTP + WebSocket)");

    Ok(handle)
}
