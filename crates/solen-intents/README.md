# solen-intents

Intent-aware execution system. Users express desired outcomes; solvers compete to fulfill them.

## Concepts

- **Intent** — a signed declaration of constraints (e.g., "transfer at least 500 tokens to Bob") with a tip incentive
- **Solver** — a service that produces `UserOperation`s to fulfill intents
- **IntentPool** — collects intents, accepts solver solutions, selects the best one

## Constraint Types

| Constraint | Description |
|-----------|-------------|
| `MinBalance` | Account must have at least N tokens after execution |
| `MaxSpend` | Account must not spend more than N tokens |
| `RequireTransfer` | A specific transfer must occur |
| `RequireCall` | A specific contract method must be called |
| `Custom` | Evaluated by a verifier contract |

## Usage

```rust
use solen_intents::pool::IntentPool;
use solen_intents::types::{Intent, Constraint, Solution};
use solen_intents::solver::{DirectTransferSolver, IntentSolver};

let pool = IntentPool::new(1000);
let id = pool.submit(intent)?;

// Solver produces a solution
let solver = DirectTransferSolver { id: solver_account };
if let Some(solution) = solver.solve(&intent) {
    pool.submit_solution(solution)?;
}

// Select best and fulfill
let best = pool.select_best_solution(id)?;
pool.fulfill(id)?;
```
