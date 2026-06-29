/**
 * Solen Wallet SDK
 *
 * Client library for interacting with the Solen network.
 */

export { SolenClient, stringifyWithBigInt } from "./client";
export { SmartAccount } from "./account";
export { PasskeyAuth } from "./auth";
export { hexToBytes, bytesToHex, nameToAccountId, nameToHex } from "./utils";
export {
  signingMessage,
  signOperationEd25519,
  ed25519PublicKey,
  signOperationMlDsa,
  verifyMlDsa,
  mlDsaKeygenFromSeed,
} from "./signing";
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
