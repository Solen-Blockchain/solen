/** SDK configuration. */
export interface SolenConfig {
  rpcUrl: string;
}

/** A 32-byte identifier represented as hex string. */
export type AccountId = string;

/**
 * A single action within a user operation.
 *
 * H-13: `amount` is the on-chain u128 and MUST be `bigint`. Using JS `number`
 * silently loses precision above 2^53−1, so large transfers would encode the
 * wrong value. The client serializes bigints as full-precision integer literals.
 */
export type Action =
  | { Transfer: { to: AccountId; amount: bigint } }
  | { Call: { target: AccountId; method: string; args: number[] } }
  | { Deploy: { code: number[]; salt: number[] } };

/** A user operation submitted to the network. */
export interface UserOperation {
  sender: number[];
  nonce: number;
  actions: Action[];
  /** On-chain u128 fee cap — `bigint` to avoid precision loss (H-13). */
  max_fee: bigint;
  signature: number[];
}

/** Account info returned by the RPC. */
export interface AccountInfo {
  id: string;
  balance: string;
  nonce: number;
  code_hash: string;
}

/** Block info returned by the RPC. */
export interface BlockInfo {
  height: number;
  epoch: number;
  parent_hash: string;
  state_root: string;
  transactions_root: string;
  receipts_root: string;
  proposer: string;
  timestamp_ms: number;
  tx_count: number;
  gas_used: number;
}

/** Chain status. */
export interface ChainStatus {
  height: number;
  latest_state_root: string;
  pending_ops: number;
}

/** Simulation result. */
export interface SimulationResult {
  success: boolean;
  gas_used: number;
  error: string | null;
  events: { emitter: string; topic: string }[];
}

/** Submit result. */
export interface SubmitResult {
  accepted: boolean;
  error: string | null;
}

/**
 * Result of `solen_submitOperationConfirm`. Combines submit-side outcome with
 * on-chain inclusion data once the op lands in a finalized block.
 *
 * - `accepted`: mempool took the op (passed nonce/balance/rate-limit checks).
 * - `confirmed`: the matching (sender, nonce) was seen in a finalized block
 *   before timeout. False if not accepted, or if the wait timed out.
 * - `success`: on-chain execution succeeded. A reverted tx is
 *   `confirmed: true, success: false` — do NOT credit funds on revert.
 * - `block_height` / `tx_hash` / `gas_used`: only meaningful when
 *   `confirmed` is true.
 */
export interface SubmitConfirmResult {
  accepted: boolean;
  confirmed: boolean;
  success: boolean;
  block_height: number;
  tx_hash: string;
  sender: string;
  nonce: number;
  gas_used: number;
  error: string | null;
}

/** JSON-RPC request. */
export interface JsonRpcRequest {
  jsonrpc: "2.0";
  method: string;
  params: unknown[];
  id: number;
}

/** JSON-RPC response. */
export interface JsonRpcResponse<T> {
  jsonrpc: "2.0";
  id: number;
  result?: T;
  error?: { code: number; message: string };
}
