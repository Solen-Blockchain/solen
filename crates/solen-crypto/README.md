# solen-crypto

Cryptographic primitives for the Solen protocol: Ed25519 signing and BLAKE3 hashing.

## API

```rust
use solen_crypto::{Keypair, verify, blake3_hash};

// Generate a keypair
let kp = Keypair::generate();

// From a deterministic seed
let kp = Keypair::from_seed(&[42u8; 32]);

// Sign and verify
let sig = kp.sign(b"message");
assert!(verify(&kp.public_key(), b"message", &sig).is_ok());

// Hash
let hash: [u8; 32] = blake3_hash(b"data");
```

## Exports

| Item | Description |
|------|-------------|
| `Keypair` | Ed25519 keypair (generate, from_seed, sign, public_key) |
| `verify(pubkey, message, signature)` | Verify an Ed25519 signature |
| `blake3_hash(data)` | BLAKE3 hash → `[u8; 32]` |
| `SigningError` | Error type for verification failures |
