/**
 * Hex and byte conversion utilities.
 */

/** Convert a hex string to a byte array. */
export function hexToBytes(hex: string): number[] {
  const clean = hex.startsWith("0x") ? hex.slice(2) : hex;
  const bytes: number[] = [];
  for (let i = 0; i < clean.length; i += 2) {
    bytes.push(parseInt(clean.substring(i, i + 2), 16));
  }
  return bytes;
}

/** Convert a byte array to a hex string. */
export function bytesToHex(bytes: number[] | Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/** Create a 32-byte account ID from a human-readable name (padded with zeros). */
export function nameToAccountId(name: string): number[] {
  const bytes = new Array(32).fill(0);
  const encoder = new TextEncoder();
  const encoded = encoder.encode(name);
  for (let i = 0; i < Math.min(encoded.length, 32); i++) {
    bytes[i] = encoded[i];
  }
  return bytes;
}

/** Create a hex account ID from a name. */
export function nameToHex(name: string): string {
  return bytesToHex(nameToAccountId(name));
}
