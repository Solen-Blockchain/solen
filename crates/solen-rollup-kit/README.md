# solen-rollup-kit

Framework for building rollup execution domains that settle on Solen L1.

## Components

| Module | Description |
|--------|-------------|
| `sequencer` | Orders L2 transactions and produces batches |
| `batch` | Compresses batches and prepares L1 commitments |
| `prover` | Pluggable proof system interface + mock prover |
| `messenger` | Cross-domain messaging with replay protection and timeouts |
| `relayer` | Monitors L1 events and relays deposits/withdrawals to L2 |

## Usage

```rust
use solen_rollup_kit::sequencer::{Sequencer, SequencerConfig, L2Transaction};
use solen_rollup_kit::batch::BatchPublisher;
use solen_rollup_kit::prover::{MockProver, ProverBackend};

// Create sequencer
let seq = Sequencer::new(SequencerConfig { rollup_id: 1, ..Default::default() });
seq.submit(tx)?;

// Produce batch
let batch = seq.produce_batch().unwrap();

// Generate proof
let prover = MockProver;
let proof = prover.generate_proof(&pre_root, &post_root, &batch_data)?;

// Prepare L1 commitment
let publisher = BatchPublisher::new(1);
let commitment = publisher.prepare_commitment(&batch, pre_root, post_root, proof)?;
```

## Cross-Domain Messaging

```rust
use solen_rollup_kit::messenger::CrossDomainMessenger;

let mut messenger = CrossDomainMessenger::new();
let id = messenger.send_message(source, dest, sender, payload, timeout_block);
messenger.execute_message(id, current_block)?;
```
