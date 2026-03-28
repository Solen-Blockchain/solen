# solen-wallet-sdk

Rust SDK for building wallets and backend services that interact with the Solen network. Provides smart account management, Ed25519 authentication, fee sponsorship, account recovery, and spending policies.

> **Status:** Early development. Core types and module structure are defined. Implementations are in progress. For a fully functional client SDK today, see the [TypeScript SDK](../wallet-sdk-ts/).

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
solen-wallet-sdk = { path = "sdks/wallet-sdk-rs" }
```

Or from the workspace:

```toml
solen-wallet-sdk = { workspace = true }
```

## Module Overview

| Module | Struct | Purpose |
|--------|--------|---------|
| `account` | `SmartAccountBuilder` | Create and configure smart accounts |
| `auth` | `PasskeyAuthenticator` | Passkey (WebAuthn) and Ed25519 authentication |
| `sponsor` | `SponsorshipPolicy` | Fee sponsorship and paymaster integration |
| `recovery` | `RecoveryManager` | Guardian-based, threshold, and timelocked recovery |
| `policy` | `PolicyEngine` | Spending limits, session credentials, approval rules |

---

## API Reference

### `account::SmartAccountBuilder`

Builder for constructing and deploying smart accounts on the Solen network.

```rust
use solen_wallet_sdk::account::SmartAccountBuilder;
```

**Planned API:**

```rust
let account = SmartAccountBuilder::new()
    .with_owner(public_key)
    .with_guardian(guardian_id)
    .with_spending_limit(1_000_000, Duration::from_secs(86400))
    .with_session_key(session_key, expiry)
    .build()?;
```

---

### `auth::PasskeyAuthenticator`

Handles WebAuthn passkey registration and assertion for smart account authentication.

```rust
use solen_wallet_sdk::auth::PasskeyAuthenticator;
```

**Planned API:**

```rust
// Server-side: generate a registration challenge
let challenge = PasskeyAuthenticator::registration_challenge(&account_id);

// Server-side: verify a registration response
let credential = PasskeyAuthenticator::verify_registration(
    &challenge,
    &attestation_response,
)?;

// Server-side: verify an assertion (login/signing)
let verified = PasskeyAuthenticator::verify_assertion(
    &credential,
    &assertion_response,
    &expected_challenge,
)?;
```

---

### `sponsor::SponsorshipPolicy`

Defines fee sponsorship rules. A paymaster contract can cover gas fees on behalf of users based on configurable policies.

```rust
use solen_wallet_sdk::sponsor::SponsorshipPolicy;
```

**Planned API:**

```rust
let policy = SponsorshipPolicy::new()
    .sponsor_account(dapp_account_id)
    .max_gas_per_op(50_000)
    .max_daily_spend(1_000_000)
    .allowed_targets(vec![contract_a, contract_b])
    .build();

// Check if an operation qualifies for sponsorship
let eligible = policy.check(&user_operation)?;
```

---

### `recovery::RecoveryManager`

Manages account recovery flows. Supports guardian-based recovery, threshold schemes, and timelocked recovery.

```rust
use solen_wallet_sdk::recovery::RecoveryManager;
```

**Planned API:**

```rust
// Set up guardians for an account
let recovery = RecoveryManager::new(account_id)
    .add_guardian(guardian_1, weight: 1)
    .add_guardian(guardian_2, weight: 1)
    .add_guardian(guardian_3, weight: 1)
    .set_threshold(2)  // 2-of-3 required
    .set_timelock(Duration::from_secs(86400 * 3))  // 3-day delay
    .build();

// Initiate recovery
let request = recovery.initiate(new_owner_key)?;

// Guardian approves
recovery.approve(&request.id, &guardian_1_signature)?;
recovery.approve(&request.id, &guardian_2_signature)?;

// Execute after timelock
recovery.execute(&request.id)?;
```

---

### `policy::PolicyEngine`

Enforces spending policies, session credentials, and approval rules on smart accounts.

```rust
use solen_wallet_sdk::policy::PolicyEngine;
```

**Planned API:**

```rust
let engine = PolicyEngine::new()
    // Daily spending limit
    .add_rule(SpendingLimit {
        max_amount: 10_000,
        window: Duration::from_secs(86400),
    })
    // Require multi-sig for large transfers
    .add_rule(ThresholdApproval {
        amount_threshold: 50_000,
        required_signers: 2,
    })
    // Session key with expiry and scope
    .add_rule(SessionKey {
        key: session_public_key,
        expires_at: timestamp,
        allowed_actions: vec![ActionType::Transfer],
        max_amount_per_tx: 1_000,
    })
    .build();

// Evaluate an operation against policies
let decision = engine.evaluate(&user_operation)?;
match decision {
    PolicyDecision::Allow => { /* proceed */ }
    PolicyDecision::RequireApproval(signers) => { /* collect signatures */ }
    PolicyDecision::Deny(reason) => { /* reject */ }
}
```

---

## Shared Types

The SDK re-exports core types from `solen-types` and `solen-crypto`:

```rust
use solen_wallet_sdk::prelude::*; // (planned)

// From solen-types
use solen_types::AccountId;          // [u8; 32]
use solen_types::Hash;               // [u8; 32]
use solen_types::account::Account;
use solen_types::account::AuthMethod;
use solen_types::transaction::UserOperation;
use solen_types::transaction::Action;
use solen_types::transaction::Intent;

// From solen-crypto
use solen_crypto::Keypair;
use solen_crypto::verify;
use solen_crypto::blake3_hash;
```

### `AuthMethod` variants

```rust
enum AuthMethod {
    Passkey { credential_id: Vec<u8> },
    Ed25519 { public_key: [u8; 32] },
    Threshold { signers: Vec<[u8; 32]>, threshold: u16 },
    Guardian { guardian_id: AccountId },
}
```

### `Action` variants

```rust
enum Action {
    Transfer { to: AccountId, amount: u128 },
    Call { target: AccountId, method: String, args: Vec<u8> },
    Deploy { code: Vec<u8>, salt: [u8; 32] },
}
```

---

## Examples

### Sign and verify a message

```rust
use solen_crypto::{Keypair, verify};

// Generate a new keypair
let kp = Keypair::generate();
println!("Public key: {:?}", kp.public_key());

// Sign a message
let message = b"hello solen";
let signature = kp.sign(message);

// Verify
assert!(verify(&kp.public_key(), message, &signature).is_ok());
```

### Deterministic keypair from a seed

```rust
use solen_crypto::Keypair;

let seed = [42u8; 32];
let kp = Keypair::from_seed(&seed);

// Same seed always produces the same keypair
let kp2 = Keypair::from_seed(&seed);
assert_eq!(kp.public_key(), kp2.public_key());
```

### Build a user operation

```rust
use solen_types::transaction::{Action, UserOperation};
use solen_crypto::Keypair;

let kp = Keypair::from_seed(&[10u8; 32]);

let mut alice_id = [0u8; 32];
alice_id[..5].copy_from_slice(b"alice");

let mut bob_id = [0u8; 32];
bob_id[..3].copy_from_slice(b"bob");

let op = UserOperation {
    sender: alice_id,
    nonce: 0,
    actions: vec![
        Action::Transfer {
            to: bob_id,
            amount: 500,
        },
    ],
    max_fee: 10_000,
    signature: vec![], // sign before submitting
};
```

### Hash data with BLAKE3

```rust
use solen_crypto::blake3_hash;

let hash = blake3_hash(b"some data");
// hash is [u8; 32]
```

---

## Crate Dependencies

| Dependency | Purpose |
|-----------|---------|
| `solen-types` | Core protocol types (accounts, transactions, blocks) |
| `solen-crypto` | Ed25519 signing, BLAKE3 hashing |
| `serde` | Serialization |
| `serde_json` | JSON encoding for RPC |
| `thiserror` | Error types |

---

## Roadmap

- [ ] RPC client (JSON-RPC transport matching the TypeScript SDK)
- [ ] Account builder with full configuration
- [ ] Passkey registration and assertion verification
- [ ] Fee sponsorship policy evaluation
- [ ] Guardian-based recovery flows
- [ ] Session key management
- [ ] Spending policy engine
- [ ] Transaction signing helpers (operation message construction)
- [ ] Intent submission support

---

## License

MIT OR Apache-2.0
