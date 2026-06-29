import { describe, it, expect } from "vitest";
import { readFileSync, writeFileSync } from "node:fs";
import { ml_dsa65 } from "@noble/post-quantum/ml-dsa";
import {
  signingMessage,
  mlDsaKeygenFromSeed,
  signOperationMlDsa,
  verifyMlDsa,
} from "./signing";
import { hexToBytes, bytesToHex } from "./utils";
import type { UserOperation } from "./types";

const V = JSON.parse(readFileSync(new URL("../test/pq_vectors.json", import.meta.url), "utf8"));

// Rebuild the canonical op (same values the Rust vector generator used).
function canonicalOp(): UserOperation {
  return {
    sender: V.op.sender,
    nonce: V.op.nonce,
    actions: [
      { Transfer: { to: V.op.actions[0].Transfer.to, amount: BigInt(V.op.actions[0].Transfer.amount) } },
      { Call: { target: V.op.actions[1].Call.target, method: V.op.actions[1].Call.method, args: V.op.actions[1].Call.args } },
    ],
    max_fee: BigInt(V.op.max_fee),
    signature: [],
  };
}

describe("TS signer ↔ Rust node parity (cross-implementation vectors)", () => {
  it("signing message digest is byte-identical to the Rust node", () => {
    const msg = signingMessage(canonicalOp(), V.chain_id);
    expect(bytesToHex(msg)).toBe(V.signing_message_hex);
  });

  it("ML-DSA-65 keygen-from-seed matches the Rust wallet's public key", () => {
    const { publicKey } = mlDsaKeygenFromSeed(Uint8Array.from(hexToBytes(V.ml_dsa_seed_hex)));
    expect(bytesToHex(publicKey)).toBe(V.ml_dsa_pubkey_hex);
  });

  it("verifies a signature produced by the Rust node (Rust → TS)", () => {
    const op = canonicalOp();
    op.signature = hexToBytes(V.ml_dsa_rust_sig_hex);
    expect(verifyMlDsa(op, Uint8Array.from(hexToBytes(V.ml_dsa_pubkey_hex)), V.chain_id)).toBe(true);
  });

  it("a TS-produced ML-DSA signature self-verifies, and is emitted for Rust to check (TS → Rust)", () => {
    const { publicKey, secretKey } = mlDsaKeygenFromSeed(Uint8Array.from(hexToBytes(V.ml_dsa_seed_hex)));
    expect(bytesToHex(publicKey)).toBe(V.ml_dsa_pubkey_hex);
    const op = canonicalOp();
    signOperationMlDsa(op, secretKey, V.chain_id);
    expect(op.signature.length).toBe(3309);
    expect(verifyMlDsa(op, publicKey, V.chain_id)).toBe(true);
    // Emit the TS signature so the Rust side can confirm TS → Rust verification.
    writeFileSync("/tmp/ts_ml_dsa_sig.hex", bytesToHex(op.signature));
  });
});
