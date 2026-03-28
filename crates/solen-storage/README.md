# solen-storage

State storage abstraction with pluggable backends.

## Backends

| Backend | Feature | Persistence | Use case |
|---------|---------|-------------|----------|
| `MemoryStore` | (default) | No | Testing, devnet |
| `RocksStore` | `rocksdb` | Yes | Production |

## StateStore Trait

```rust
pub trait StateStore: Send + Sync {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<(), StorageError>;
    fn delete(&mut self, key: &[u8]) -> Result<(), StorageError>;
    fn state_root(&self) -> Hash;        // BLAKE3 Merkle root
    fn snapshot(&self) -> Box<dyn StateStore>;  // Copy for simulation
    fn len(&self) -> usize;
}
```

## Usage

```rust
use solen_storage::{MemoryStore, StateStore};

let mut store = MemoryStore::new();
store.put(b"key", b"value").unwrap();
let root = store.state_root(); // deterministic Merkle root
```

Enable RocksDB with `features = ["rocksdb"]` in Cargo.toml.
