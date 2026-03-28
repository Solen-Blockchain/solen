# Spec 001: Terminology

**Status:** Draft

## Definitions

- **Settlement Layer (L1):** The Solen base chain responsible for consensus, finality, and canonical state.
- **Execution Domain:** A rollup or app-specific environment that settles to Solen L1.
- **Smart Account:** A programmable account with authentication policies; the only account type in Solen.
- **User Operation:** A signed action bundle submitted by a smart account.
- **Intent:** A declarative expression of a desired outcome, resolved by solvers/bundlers.
- **Batch Commitment:** A state root and proof published by a rollup sequencer to L1.
- **Epoch:** A fixed interval of blocks used for validator rotation and reward distribution.
