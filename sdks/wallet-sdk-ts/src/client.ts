/**
 * JSON-RPC client for communicating with a Solen node.
 */
import type {
  SolenConfig,
  AccountInfo,
  BlockInfo,
  ChainStatus,
  SimulationResult,
  SubmitResult,
  SubmitConfirmResult,
  UserOperation,
  JsonRpcRequest,
  JsonRpcResponse,
} from "./types";

/**
 * JSON.stringify that emits `bigint` values as full-precision integer literals
 * (e.g. `"amount":340282366920938463463374607431768211455`) instead of throwing.
 * The node deserializes u128 amount/max_fee from JSON numbers, and serde parses
 * arbitrary-precision integer literals into u128 — so this preserves the exact
 * value where plain `number` would round above 2^53−1 (H-13). The replacer wraps
 * each bigint in a unique alphanumeric marker (JSON-safe, never escaped); the
 * surrounding quotes are then stripped. The marker only ever encloses `-?\d+`,
 * so the unquoting regex cannot match ordinary string data.
 */
export function stringifyWithBigInt(value: unknown): string {
  const MARK = "zZbigIntZz";
  const json = JSON.stringify(value, (_key, v) =>
    typeof v === "bigint" ? `${MARK}${v}${MARK}` : v,
  );
  return json.replace(new RegExp(`"${MARK}(-?\\d+)${MARK}"`, "g"), "$1");
}

export class SolenClient {
  private url: string;
  private nextId = 1;

  constructor(config: SolenConfig) {
    this.url = config.rpcUrl;
  }

  private async call<T>(method: string, params: unknown[] = []): Promise<T> {
    const request: JsonRpcRequest = {
      jsonrpc: "2.0",
      method,
      params,
      id: this.nextId++,
    };

    const response = await fetch(this.url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      // H-13: bigint-safe serialization so u128 amounts/fees keep full precision.
      body: stringifyWithBigInt(request),
    });

    if (!response.ok) {
      throw new Error(`HTTP ${response.status}: ${response.statusText}`);
    }

    const json: JsonRpcResponse<T> = await response.json();

    if (json.error) {
      throw new Error(`RPC error ${json.error.code}: ${json.error.message}`);
    }

    return json.result as T;
  }

  /** Get the balance of an account. */
  async getBalance(accountId: string): Promise<bigint> {
    const result = await this.call<string>("solen_getBalance", [accountId]);
    return BigInt(result);
  }

  /** Get full account info. */
  async getAccount(accountId: string): Promise<AccountInfo> {
    return this.call<AccountInfo>("solen_getAccount", [accountId]);
  }

  /** Get a block by height. */
  async getBlock(height: number): Promise<BlockInfo> {
    return this.call<BlockInfo>("solen_getBlock", [height]);
  }

  /** Get the latest block. */
  async getLatestBlock(): Promise<BlockInfo> {
    return this.call<BlockInfo>("solen_getLatestBlock", []);
  }

  /** Get chain status. */
  async chainStatus(): Promise<ChainStatus> {
    return this.call<ChainStatus>("solen_chainStatus", []);
  }

  /** Submit a signed user operation. */
  async submitOperation(op: UserOperation): Promise<SubmitResult> {
    return this.call<SubmitResult>("solen_submitOperation", [op]);
  }

  /**
   * Submit a signed user operation and wait for it to be included in a
   * finalized block. Returns once the matching (sender, nonce) lands or after
   * the timeout (default 60s, max 180s server-side). Designed for exchange
   * integrations that want a single round-trip with full confirmation data.
   *
   * Important: a reverted on-chain tx returns `confirmed: true, success: false`.
   * Do not credit funds on revert.
   */
  async submitOperationConfirm(
    op: UserOperation,
    timeoutSecs?: number,
  ): Promise<SubmitConfirmResult> {
    return this.call<SubmitConfirmResult>("solen_submitOperationConfirm", [
      op,
      timeoutSecs ?? null,
    ]);
  }

  /** Simulate a user operation without modifying state. */
  async simulateOperation(op: UserOperation): Promise<SimulationResult> {
    return this.call<SimulationResult>("solen_simulateOperation", [op]);
  }
}
