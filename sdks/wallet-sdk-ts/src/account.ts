/**
 * Smart account management and transaction building.
 */
import { SolenClient } from "./client";
import type {
  AccountInfo,
  Action,
  SimulationResult,
  SubmitResult,
  UserOperation,
} from "./types";
import { hexToBytes, bytesToHex } from "./utils";

export class SmartAccount {
  readonly id: string;
  private idBytes: number[];
  private client: SolenClient;

  constructor(accountIdHex: string, client: SolenClient) {
    this.id = accountIdHex;
    this.idBytes = hexToBytes(accountIdHex);
    this.client = client;
  }

  /** Get account info from the chain. */
  async getInfo(): Promise<AccountInfo> {
    return this.client.getAccount(this.id);
  }

  /** Get current balance. */
  async getBalance(): Promise<bigint> {
    return this.client.getBalance(this.id);
  }

  /** Get current nonce. */
  async getNonce(): Promise<number> {
    const info = await this.getInfo();
    return info.nonce;
  }

  /**
   * Build a transfer operation (unsigned).
   *
   * `amount` and `maxFee` are `bigint` (the on-chain u128) — pass `10n`, not
   * `10`. Using `number` would lose precision above 2^53−1 (H-13).
   */
  async buildTransfer(
    toHex: string,
    amount: bigint,
    maxFee: bigint = 10000n
  ): Promise<UserOperation> {
    const nonce = await this.getNonce();
    return {
      sender: this.idBytes,
      nonce,
      actions: [{ Transfer: { to: toHex, amount } }],
      max_fee: maxFee,
      signature: [],
    };
  }

  /** Build a contract call operation (unsigned). */
  async buildCall(
    targetHex: string,
    method: string,
    args: number[] = [],
    maxFee: bigint = 50000n
  ): Promise<UserOperation> {
    const nonce = await this.getNonce();
    return {
      sender: this.idBytes,
      nonce,
      actions: [{ Call: { target: targetHex, method, args } }],
      max_fee: maxFee,
      signature: [],
    };
  }

  /** Build a deploy operation (unsigned). */
  async buildDeploy(
    code: number[],
    salt: number[],
    maxFee: bigint = 100000n
  ): Promise<UserOperation> {
    const nonce = await this.getNonce();
    return {
      sender: this.idBytes,
      nonce,
      actions: [{ Deploy: { code, salt } }],
      max_fee: maxFee,
      signature: [],
    };
  }

  /** Simulate an operation without modifying state. */
  async simulate(op: UserOperation): Promise<SimulationResult> {
    return this.client.simulateOperation(op);
  }

  /** Submit a signed operation. */
  async submit(op: UserOperation): Promise<SubmitResult> {
    return this.client.submitOperation(op);
  }
}
