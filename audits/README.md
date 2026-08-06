# Audits

This directory holds third-party security audit reports for Solen. It exists so
that audit artifacts are versioned alongside the code they cover.

## Current status (honest disclosure)

**No independent third-party security firm has audited Solen yet.** We state this
plainly because prior materials were not clear enough about it.

What review *has* been done, and what it was:

- **Internal engineering review and adversarial testing** by the core team,
  including AI-assisted adversarial review (multiple rounds of tool-driven code
  review). This is *not* a substitute for an independent audit, and must not be
  described as an "independent security review." It surfaced real issues that
  were fixed, but it is self-review by construction.
- **Live incident response and public post-mortems** for consensus issues
  encountered on mainnet (see [`../docs/postmortems/`](../docs/postmortems/)).
  These are disclosed transparently, with root causes and fixes.
- **Extensive test suite and fuzz targets** in-repo (`cargo test --workspace`,
  `fuzz/`). Tests are a floor, not an audit.

## Planned audit scope

When engaged, an independent audit should cover, in priority order:

1. **Consensus safety & liveness** — BFT proof-of-stake engine, fork choice,
   finality, and the epoch/proposer-selection determinism (including the
   post-mortem'd incident classes and the epoch-randomness design).
2. **Execution determinism & state integrity** — state-transition function,
   state-root computation, and supply/balance invariants.
3. **Post-quantum & hybrid authentication** — ML-DSA-65 (FIPS-204) integration,
   hybrid verification, and address-preserving key rotation (downgrade and
   malleability analysis).
4. **System contracts** — staking, governance (including the dormant emergency
   fast-track gate), vesting, and the Base bridge / wrapped-asset custody model.
5. **Networking & RPC** — sync-path authentication and remotely triggerable DoS.

## Process

- Audit engagement letters and scopes will be linked here when signed.
- Final reports (and our remediation responses) are published in this directory
  as they complete, named `YYYY-MM-<firm>-<scope>.pdf` with a summary in this
  README.
- We will not describe any AI-assisted or internal review as an independent
  audit.

_Last updated: 2026-08. Status: pre-audit._
