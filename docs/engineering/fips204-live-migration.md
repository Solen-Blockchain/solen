# Activating post-quantum account auth on a live chain (FIPS-204 / ML-DSA-65)

Solen added **post-quantum account authentication** — NIST FIPS-204 ML-DSA-65 —
to a live mainnet via a coordinated, opt-in, address-preserving activation, with
no forced migration and no disruption to classical accounts. This is a write-up
of how, because "quantum-safe" is easy to put on a slide and hard to ship
without forking your own chain.

## Why

Account signatures are the one place where "harvest now, decrypt later" is
directly relevant: an adversary recording today's transactions could forge
signatures once large quantum computers exist. Ed25519 alone does not defend
against that. The goal was to let any account move to a quantum-resistant scheme
**without** breaking existing accounts, changing addresses, or requiring a hard
fork.

## Design

**Auth methods per account.** An account's authorizer can be `Ed25519`,
`MlDsa` (ML-DSA-65 only), or **`Hybrid`** (Ed25519 **and** ML-DSA-65 — a
signature must satisfy *both* to verify). Hybrid is the recommended transition
state: it is no weaker than Ed25519 today and adds PQ security, so a bug or
weakness in either scheme alone cannot forge a transaction.

**Address preservation.** Rotating to `Hybrid`/`MlDsa` keeps the same account id.
Users don't get a new address; integrations, balances, and history are untouched.
The wallet path is phrase-preserving (the account id continues to equal the
Ed25519 key), so a user upgrades from their existing seed phrase.

**Opt-in, no forced migration.** 100% classical Ed25519 accounts keep working
unchanged. PQ is adopted per-account via a normal `SetAuth`, on each holder's
schedule.

## Activation: a height-gated flag-day

PQ verification is **consensus-affecting** (it changes which operations are
valid), so it cannot simply "turn on" per node — nodes must agree. It ships
behind a single height gate, `--pq-auth-height` (default `u64::MAX` = dormant):

- **Below the height:** `MlDsa`/`Hybrid` auth methods verify as *no valid
  method* — exactly as they would on a node that lacked the feature. A node
  running the PQ-capable binary with the flag unset is byte-for-byte
  behaviourally identical to one without it, so the binary rolls out as a routine
  dormant deploy with no fork risk.
- **At/after the height:** every node honours ML-DSA / hybrid signatures.

Activation is therefore: (1) deploy the capable binary everywhere, dormant; (2)
pick a future height `H`; (3) set `--pq-auth-height H` on **every** node and
confirm all report `H` (exposed via `chainStatus.config.pq_auth_height`) *before*
the chain reaches `H`. Because behaviour is identical below `H`, restart order
doesn't matter and there is no divergence window — the switch is simultaneous by
construction. `verify_ml_dsa` is a deterministic pure function, so it is not in
the non-deterministic-execution risk class.

## Cross-stack signing parity

ML-DSA-65 is **hedged/randomized** — two correct signers over the same message
produce *different* signatures by design. So the cross-implementation test
(Rust node ↔ TypeScript wallet) asserts **mutual verification**, not byte
equality: each side verifies the other's signatures, and digest/keygen-from-seed
parity is checked with fixed vectors. A naive `rust_sig == ts_sig` comparison
would be wrong and would red-flag a correct implementation.

## Live canary

Before broad use, we canaried the full path on mainnet after the flag-day:

1. Rotate a funded account `Ed25519 → Hybrid` (address unchanged).
2. Submit a **hybrid-signed** transfer.
3. Confirm inclusion and that **every** node agrees on the resulting state root
   (no fork from the first PQ operation), and the wallet's security view flips to
   "active."

Both operations landed with the fleet unanimous on state root — end-to-end proof
that PQ auth works and is deterministic across the network, not just in a unit
test.

## Honest limitations

- **Not yet independently audited.** The ML-DSA integration, hybrid verification,
  and address-preserving rotation are exactly where subtle auth-bypass or
  downgrade bugs live; this code is a priority item for an external audit (see
  `audits/README.md`). Treat "quantum-safe" as "quantum-*ready*, pending
  third-party review."
- **Signatures are larger.** ML-DSA-65 signatures/keys are substantially bigger
  than Ed25519; hybrid transactions carry both. That is a deliberate
  size-for-security trade during the transition.
- **PQ does not drive user demand today.** This is infrastructure hygiene and a
  differentiator for security-sensitive/custody use cases, not a growth lever on
  its own.

## Takeaway

The interesting engineering claim is not "we use ML-DSA" — libraries exist. It is
that a live chain performed a **coordinated, opt-in, address-preserving PQ
activation with a zero-divergence flag-day and an on-chain canary**, while
leaving every classical account untouched. That migration pattern
(dormant-deploy → same-height activation → hybrid transition → on-chain
verification) is reusable for any consensus-affecting upgrade.
