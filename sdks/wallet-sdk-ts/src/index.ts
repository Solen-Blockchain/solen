/**
 * Solen Wallet SDK
 *
 * Client library for interacting with the Solen network.
 */

export { SolenClient } from "./client";
export { SmartAccount } from "./account";
export { PasskeyAuth } from "./auth";
export { hexToBytes, bytesToHex, nameToAccountId, nameToHex } from "./utils";
export type {
  SolenConfig,
  AccountId,
  Action,
  UserOperation,
  AccountInfo,
  BlockInfo,
  ChainStatus,
  SimulationResult,
  SubmitResult,
} from "./types";
