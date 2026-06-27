import { describe, it, expect } from "vitest";
import { stringifyWithBigInt } from "./client";

describe("stringifyWithBigInt (H-13: u128 precision)", () => {
  it("emits a bigint as a bare full-precision integer literal (no quotes, no rounding)", () => {
    const u128Max = (1n << 128n) - 1n;
    const json = stringifyWithBigInt({ amount: u128Max });
    // Exact digits, unquoted — serde parses this into u128.
    expect(json).toBe(`{"amount":${u128Max.toString()}}`);
    expect(json).not.toContain(`"${u128Max.toString()}"`); // not a string
  });

  it("preserves values above Number.MAX_SAFE_INTEGER that JS number would round", () => {
    const big = 1_000_000_000_000_000_000_000n; // 1e21, > 2^53
    const json = stringifyWithBigInt({ params: [{ max_fee: big }] });
    expect(json).toContain(`"max_fee":${big.toString()}`);
    // Sanity: the naive Number path WOULD have lost precision.
    expect(Number(big).toString()).not.toBe(big.toString());
  });

  it("handles negatives and leaves ordinary fields untouched", () => {
    const json = stringifyWithBigInt({ a: -5n, b: "hi", c: 3, d: [1n, 2n] });
    expect(json).toBe(`{"a":-5,"b":"hi","c":3,"d":[1,2]}`);
  });

  it("does not corrupt a string that merely looks numeric", () => {
    const json = stringifyWithBigInt({ note: "12345" });
    expect(json).toBe(`{"note":"12345"}`); // stays a quoted string
  });
});
