//! State root determinism tests.
//!
//! Critical: MemoryStore and any other backend must produce
//! identical state roots for identical data. Non-power-of-2
//! entry counts are the known-buggy cases (was a real consensus bug).

use solen_storage::{MemoryStore, StateStore};

fn write_entries(store: &mut dyn StateStore, count: usize) {
    for i in 0..count {
        let key = format!("key_{:05}", i);
        let val = format!("val_{:05}", i);
        store.put(key.as_bytes(), val.as_bytes()).unwrap();
    }
}

// ── Test #13: Merkle root for non-power-of-2 entries ──────────

#[test]
fn merkle_root_3_entries_deterministic() {
    let mut store = MemoryStore::new();
    write_entries(&mut store, 3);
    let root = store.state_root();
    assert_ne!(root, [0u8; 32]);

    // Same data in a different store — must match.
    let mut store2 = MemoryStore::new();
    write_entries(&mut store2, 3);
    assert_eq!(
        store.state_root(),
        store2.state_root(),
        "3-entry merkle roots must match"
    );
}

#[test]
fn merkle_root_5_entries_deterministic() {
    let mut a = MemoryStore::new();
    let mut b = MemoryStore::new();
    write_entries(&mut a, 5);
    write_entries(&mut b, 5);
    assert_eq!(a.state_root(), b.state_root());
}

#[test]
fn merkle_root_7_entries_deterministic() {
    let mut a = MemoryStore::new();
    let mut b = MemoryStore::new();
    write_entries(&mut a, 7);
    write_entries(&mut b, 7);
    assert_eq!(a.state_root(), b.state_root());
}

#[test]
fn merkle_root_11_entries_deterministic() {
    let mut a = MemoryStore::new();
    let mut b = MemoryStore::new();
    write_entries(&mut a, 11);
    write_entries(&mut b, 11);
    assert_eq!(a.state_root(), b.state_root());
}

#[test]
fn merkle_root_1000_entries_deterministic() {
    let mut a = MemoryStore::new();
    let mut b = MemoryStore::new();
    write_entries(&mut a, 1000);
    write_entries(&mut b, 1000);
    assert_eq!(a.state_root(), b.state_root());
}

// ── Test: Insertion order doesn't affect root ─────────────────

#[test]
fn merkle_root_insertion_order_independent() {
    let mut forward = MemoryStore::new();
    for i in 0..10 {
        forward
            .put(format!("k{i}").as_bytes(), format!("v{i}").as_bytes())
            .unwrap();
    }

    let mut reverse = MemoryStore::new();
    for i in (0..10).rev() {
        reverse
            .put(format!("k{i}").as_bytes(), format!("v{i}").as_bytes())
            .unwrap();
    }

    assert_eq!(forward.state_root(), reverse.state_root());
}

// ── Test: Non-state keys filtered from root ───────────────────

#[test]
fn non_state_keys_excluded_from_root() {
    let mut with_meta = MemoryStore::new();
    with_meta.put(b"acc/alice", b"100").unwrap();
    let root1 = with_meta.state_root();

    // Add non-state keys — root must NOT change.
    with_meta.put(b"block/1", b"data").unwrap();
    with_meta.put(b"__chain_meta__", b"meta").unwrap();
    with_meta.put(b"__chain_id__", b"1337").unwrap();
    with_meta.put(b"slash/evidence", b"proof").unwrap();
    let root2 = with_meta.state_root();

    assert_eq!(
        root1, root2,
        "non-state keys must not affect state root"
    );
}

// ── Test: scan_all returns all entries ─────────────────────────

#[test]
fn scan_all_returns_everything() {
    let mut store = MemoryStore::new();
    write_entries(&mut store, 50);
    let all = store.scan_all().unwrap();
    assert_eq!(all.len(), 50);
}

// ── Test: scan_prefix correctness ─────────────────────────────

#[test]
fn scan_prefix_returns_only_matching() {
    let mut store = MemoryStore::new();
    store.put(b"user/alice", b"1").unwrap();
    store.put(b"user/bob", b"2").unwrap();
    store.put(b"user/charlie", b"3").unwrap();
    store.put(b"contract/xyz", b"4").unwrap();

    let users = store.scan_prefix(b"user/").unwrap();
    assert_eq!(users.len(), 3);

    let contracts = store.scan_prefix(b"contract/").unwrap();
    assert_eq!(contracts.len(), 1);

    let empty = store.scan_prefix(b"nonexistent/").unwrap();
    assert_eq!(empty.len(), 0);
}
