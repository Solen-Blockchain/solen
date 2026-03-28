/** SDK configuration. */
export interface SolenConfig {
  rpcUrl: string;
}

/** A 32-byte identifier represented as hex string. */
export type AccountId = string;

/** A single action within a user operation. */
export type Action =
  | { Transfer: { to: AccountId; amount: number } }
  | { Call: { target: AccountId; method: string; args: number[] } }
  | { Deploy: { code: number[]; salt: number[] } };

/** A user operation submitted to the network. */
export interface UserOperation {
  sender: number[];
  nonce: number;
  actions: Action[];
  max_fee: number;
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
