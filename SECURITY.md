# Security Policy

Solen is a live proof-of-stake Layer-1 handling real value. We take security
reports seriously and appreciate coordinated disclosure.

## Reporting a vulnerability

**Do not open a public issue for security vulnerabilities.**

Email **security@solenchain.io** with:

- A description of the issue and its impact.
- Steps to reproduce (proof-of-concept preferred).
- Affected component(s) and commit/tag if known.
- Your assessment of severity.

You will receive an acknowledgement within **72 hours** and a triage
assessment within **7 days**. We will keep you informed through remediation and
credit you in the release notes unless you prefer to remain anonymous.

Please do not disclose publicly until a fix has shipped and operators have had a
reasonable window to upgrade (coordinated disclosure).

## Scope

In scope — issues that can affect chain safety, liveness, or funds:

- **Consensus** (`crates/solen-consensus`): safety/liveness violations, forks,
  non-deterministic execution, proposer-selection manipulation, finality
  reversion.
- **Execution & state** (`crates/solen-execution`, `crates/solen-storage`):
  state-transition bugs, state-root divergence, balance/supply invariant
  violations, non-determinism.
- **Account authentication** including the post-quantum (ML-DSA-65 / FIPS-204)
  and hybrid auth paths (`crates/solen-crypto`, auth in `solen-execution`):
  signature bypass, downgrade attacks, key-rotation flaws.
- **System contracts** (`crates/solen-system-contracts`): staking, governance,
  vesting, bridge — privilege escalation, fund drainage, accounting errors.
- **Networking / sync** (`crates/solen-p2p`): eclipse, block/attestation
  forgery, DoS that halts the chain.
- **RPC** (`crates/solen-rpc`): remotely triggerable panics/DoS, data integrity.
- **Wallet / SDKs**: key handling, transaction-construction flaws.

Out of scope: issues requiring physical/root access to a validator host; social
engineering; third-party dependencies without a Solen-specific exploit; theoretical
issues without a practical attack; UI cosmetics.

## Bug bounty

A formal bug-bounty program is planned. Until it launches, we will still reward
material, responsibly disclosed findings at our discretion. Contact us before
testing anything against mainnet — use a local devnet or the testnet for
proof-of-concept work. Never test with resources you do not control, and never
run availability/DoS attacks against live infrastructure.

## Supported versions

Only the latest tagged release (see the GitHub Releases page and its published
SHA-256) is supported. Operators should run only tagged, CI-built binaries and
verify `sha256sum` against the release.

## Audits

See [`audits/README.md`](audits/README.md) for current audit status and the
review process. Third-party audit reports, when available, are published there.
