# Post-mortems

Public, technical write-ups of notable mainnet incidents — root cause, fix, and
lessons. We publish these deliberately: transparent, rigorous incident response
is a stronger signal than pretending a young consensus engine never breaks.

| Date | Incident | Class | Outcome |
|------|----------|-------|---------|
| 2026-07-28 | [Epoch-boundary halt from a non-canonical proposer seed](2026-07-28-epoch-boundary-halt.md) | Liveness (halt) | Fixed (seed from state_root); no fund loss, no safety violation |
| 2026-07-16 | [Single-validator state-root fork at an epoch boundary](2026-07-16-state-root-fork.md) | Safety (single-node divergence) | Recovered (snapshot-resync); quorum stayed canonical |

Related engineering write-ups live in [`../engineering/`](../engineering/)
(e.g. the FIPS-204 post-quantum live-migration case study).
