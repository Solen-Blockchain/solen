/**
 * Operation signing — classical (Ed25519) and post-quantum (ML-DSA-65).
 *
 * The signing digest MUST be byte-identical to the Rust node's
 * `UserOperation::signing_message`, or the network will reject the signature.
 * Layout (96 bytes):
 *
 *   chain_id[8 LE] ‖ sender[32] ‖ nonce[8 LE] ‖ max_fee[16 LE] ‖ blake3(serde_json(actions))[32]
 *
 * `serde_json(actions)` is replicated exactly: externally-tagged enum variants,
 * `AccountId`/byte fields as number arrays, `u128` as bare integer literals,
 * compact (no whitespace), struct fields in declaration order. Parity is pinned
 * by the cross-implementation vectors in `test/pq_vectors.json`.
 */
import { blake3 } from "@noble/hashes/blake3";
import { sha512 } from "@noble/hashes/sha512";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa";
import * as ed25519 from "@noble/ed25519";

// @noble/ed25519 v2 needs a synchronous SHA-512 hook wired in once.
ed25519.etc.sha512Sync = (...m: Uint8Array[]) => sha512(ed25519.etc.concatBytes(...m));

import { stringifyWithBigInt } from "./client";
import type { Action, UserOperation } from "./types";
import { hexToBytes } from "./utils";

function u64le(n: number | bigint): Uint8Array {
  const out = new Uint8Array(8);
  let v = BigInt(n);
  for (let i = 0; i < 8; i++) { out[i] = Number(v & 0xffn); v >>= 8n; }
  return out;
}

function u128le(n: bigint): Uint8Array {
  const out = new Uint8Array(16);
  let v = n;
  for (let i = 0; i < 16; i++) { out[i] = Number(v & 0xffn); v >>= 8n; }
  return out;
}

/** Normalize an AccountId (hex string or byte array) to a number[] of bytes. */
function idBytes(id: string | number[]): number[] {
  return typeof id === "string" ? hexToBytes(id) : id;
}

/**
 * Reproduce Rust `serde_json::to_vec(&actions)` exactly. Each action is rebuilt
 * in the node's serde shape (externally tagged, byte fields as number arrays,
 * u128 as bigint → bare integer), then compact-stringified.
 */
function serdeActions(actions: Action[]): Uint8Array {
  const mapped = actions.map((a) => {
    if ("Transfer" in a) {
      return { Transfer: { to: idBytes(a.Transfer.to), amount: a.Transfer.amount } };
    }
    if ("Call" in a) {
      return { Call: { target: idBytes(a.Call.target), method: a.Call.method, args: a.Call.args } };
    }
    if ("Deploy" in a) {
      return { Deploy: { code: a.Deploy.code, salt: a.Deploy.salt } };
    }
    throw new Error("unsupported action variant for signing");
  });
  return new TextEncoder().encode(stringifyWithBigInt(mapped));
}

/**
 * The 96-byte signing message for an operation — byte-identical to the Rust
 * `UserOperation::signing_message(chain_id)`.
 */
export function signingMessage(op: UserOperation, chainId: number | bigint): Uint8Array {
  const out = new Uint8Array(96);
  out.set(u64le(chainId), 0);
  out.set(Uint8Array.from(idBytes(op.sender)), 8);
  out.set(u64le(op.nonce), 40);
  out.set(u128le(BigInt(op.max_fee)), 48);
  out.set(blake3(serdeActions(op.actions)), 64);
  return out;
}

// ── ML-DSA-65 (post-quantum) ──────────────────────────────────────────────

/** Derive an ML-DSA-65 keypair from a 32-byte seed (matches the Rust wallet). */
export function mlDsaKeygenFromSeed(seed: Uint8Array): { publicKey: Uint8Array; secretKey: Uint8Array } {
  if (seed.length !== 32) throw new Error("seed must be 32 bytes");
  return ml_dsa65.keygen(seed);
}

/**
 * Sign an operation in place with an ML-DSA-65 secret key (post-quantum). The
 * account must be authorized by `AuthMethod::MlDsa` with the matching public key
 * and the network must have post-quantum auth active.
 */
export function signOperationMlDsa(op: UserOperation, secretKey: Uint8Array, chainId: number | bigint): void {
  const sig = ml_dsa65.sign(secretKey, signingMessage(op, chainId));
  op.signature = Array.from(sig);
}

/** Verify an ML-DSA-65 signature over an operation (for tests / dry-runs). */
export function verifyMlDsa(op: UserOperation, publicKey: Uint8Array, chainId: number | bigint): boolean {
  return ml_dsa65.verify(publicKey, signingMessage(op, chainId), Uint8Array.from(op.signature));
}

// ── Ed25519 (classical) ───────────────────────────────────────────────────

/** Sign an operation in place with a 32-byte Ed25519 secret seed. */
export function signOperationEd25519(op: UserOperation, secretSeed: Uint8Array, chainId: number | bigint): void {
  const sig = ed25519.sign(signingMessage(op, chainId), secretSeed);
  op.signature = Array.from(sig);
}

/** The Ed25519 public key for a 32-byte secret seed. */
export function ed25519PublicKey(secretSeed: Uint8Array): Uint8Array {
  return ed25519.getPublicKey(secretSeed);
}

// ── AND-hybrid (Ed25519 + ML-DSA-65) ──────────────────────────────────────

/**
 * Sign an operation in place with an AND-hybrid key (both Ed25519 and ML-DSA-65,
 * typically derived from the same 32-byte seed). Signature layout, matching the
 * node's `Hybrid` auth method: `ed25519[64] ‖ ml_dsa[3309]`. Both halves must
 * verify for the network to authorize.
 */
export function signOperationHybrid(
  op: UserOperation,
  edSecretSeed: Uint8Array,
  mlSecretKey: Uint8Array,
  chainId: number | bigint,
): void {
  const msg = signingMessage(op, chainId);
  const ed = ed25519.sign(msg, edSecretSeed);
  const ml = ml_dsa65.sign(mlSecretKey, msg);
  const sig = new Uint8Array(ed.length + ml.length);
  sig.set(ed, 0);
  sig.set(ml, ed.length);
  op.signature = Array.from(sig);
}

/** Verify a hybrid signature: BOTH the Ed25519 and ML-DSA-65 halves must pass. */
export function verifyHybrid(
  op: UserOperation,
  edPublicKey: Uint8Array,
  mlPublicKey: Uint8Array,
  chainId: number | bigint,
): boolean {
  const sig = Uint8Array.from(op.signature);
  if (sig.length !== 64 + 3309) return false;
  const msg = signingMessage(op, chainId);
  return (
    ed25519.verify(sig.slice(0, 64), msg, edPublicKey) &&
    ml_dsa65.verify(mlPublicKey, msg, sig.slice(64))
  );
}
