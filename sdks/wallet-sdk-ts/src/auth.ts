/**
 * Passkey (WebAuthn) authentication support.
 *
 * This module provides helpers for registering and authenticating
 * with passkeys in browser environments.
 */

/** Passkey credential after registration. */
export interface PasskeyCredential {
  credentialId: Uint8Array;
  publicKey: Uint8Array;
}

export class PasskeyAuth {
  /**
   * Check if WebAuthn is available in the current environment.
   */
  static isAvailable(): boolean {
    return (
      typeof globalThis !== "undefined" &&
      "PublicKeyCredential" in globalThis
    );
  }
}
