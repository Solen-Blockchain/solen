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
  UserOperation,
  JsonRpcRequest,
  JsonRpcResponse,
} from "./types";

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
      body: JSON.stringify(request),
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

  /** Simulate a user operation without modifying state. */
  async simulateOperation(op: UserOperation): Promise<SimulationResult> {
    return this.call<SimulationResult>("solen_simulateOperation", [op]);
  }
}
