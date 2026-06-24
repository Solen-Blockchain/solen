//! State snapshots for fast sync.
//!
//! Archive nodes can create compressed snapshots of the full state at a given
//! block height. New nodes download a snapshot, verify the state root, and
//! resume syncing from that height — skipping replay of the entire chain.
//!
//! Snapshot format (binary):
//!   header[48] + compressed_data
//!
//! Header:
//!   magic[4]        = "SNAP"
//!   version[4]      = 1u32 LE
//!   height[8]       = u64 LE
//!   epoch[8]        = u64 LE
//!   state_root[32]  = [u8; 32] (expected state root for verification)
//!
//! Compressed data (deflate):
//!   entry_count[8]  = u64 LE
//!   entries[]:
//!     key_len[4]    = u32 LE
//!     key[key_len]
//!     val_len[4]    = u32 LE
//!     val[val_len]

use std::io::Write;

use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use solen_storage::StateStore;
use solen_types::Hash;
use thiserror::Error;
use tracing::info;

const MAGIC: &[u8; 4] = b"SNAP";
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 56;

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("invalid snapshot: {0}")]
    Invalid(String),
    #[error("state root mismatch: expected {expected}, got {actual}")]
    StateRootMismatch { expected: String, actual: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage error: {0}")]
    Storage(String),
}

/// Metadata for a snapshot.
#[derive(Debug, Clone)]
pub struct SnapshotMeta {
    pub height: u64,
    pub epoch: u64,
    pub state_root: Hash,
    pub entry_count: u64,
    pub compressed_size: usize,
    pub uncompressed_size: usize,
}

/// Create a compressed snapshot from the current state store.
pub fn create_snapshot(
    store: &dyn StateStore,
    height: u64,
    epoch: u64,
) -> Result<Vec<u8>, SnapshotError> {
    let state_root = store.state_root();
    let entries = store.scan_all().map_err(|e| SnapshotError::Storage(e.to_string()))?;
    let entry_count = entries.len() as u64;

    // Serialize entries to uncompressed buffer.
    let mut raw = Vec::new();
    raw.extend_from_slice(&entry_count.to_le_bytes());

    for (key, val) in &entries {
        raw.extend_from_slice(&(key.len() as u32).to_le_bytes());
        raw.extend_from_slice(key);
        raw.extend_from_slice(&(val.len() as u32).to_le_bytes());
        raw.extend_from_slice(val);
    }

    let uncompressed_size = raw.len();

    // Compress.
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(&raw)?;
    let compressed = encoder.finish()?;

    // Build output: header + compressed data.
    let mut output = Vec::with_capacity(HEADER_SIZE + compressed.len());
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&VERSION.to_le_bytes());
    output.extend_from_slice(&height.to_le_bytes());
    output.extend_from_slice(&epoch.to_le_bytes());
    output.extend_from_slice(&state_root);

    // Extra header fields: compressed size and uncompressed size for info.
    output.extend_from_slice(&compressed);

    info!(
        height,
        epoch,
        entries = entry_count,
        compressed = compressed.len(),
        uncompressed = uncompressed_size,
        ratio = format!("{:.1}x", uncompressed_size as f64 / compressed.len().max(1) as f64),
        "snapshot created"
    );

    Ok(output)
}

/// Parse snapshot header without decompressing.
pub fn read_snapshot_meta(data: &[u8]) -> Result<SnapshotMeta, SnapshotError> {
    if data.len() < HEADER_SIZE {
        return Err(SnapshotError::Invalid("too short".into()));
    }
    if &data[..4] != MAGIC {
        return Err(SnapshotError::Invalid("bad magic".into()));
    }
    let version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if version != VERSION {
        return Err(SnapshotError::Invalid(format!("unsupported version: {version}")));
    }

    let height = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let epoch = u64::from_le_bytes(data[16..24].try_into().unwrap());
    let mut state_root = [0u8; 32];
    state_root.copy_from_slice(&data[24..56]);

    let compressed_size = data.len() - HEADER_SIZE;

    Ok(SnapshotMeta {
        height,
        epoch,
        state_root,
        entry_count: 0, // unknown until decompressed
        compressed_size,
        uncompressed_size: 0,
    })
}

/// Restore a snapshot into a state store. Verifies the state root matches.
pub fn restore_snapshot(
    store: &mut dyn StateStore,
    data: &[u8],
) -> Result<SnapshotMeta, SnapshotError> {
    let meta = read_snapshot_meta(data)?;
    let compressed = &data[HEADER_SIZE..];

    // Decompress with size limit to prevent decompression bombs.
    const MAX_SNAPSHOT_SIZE: usize = 2 * 1024 * 1024 * 1024; // 2 GB
    let mut decoder = DeflateDecoder::new(compressed);
    let mut raw = Vec::new();
    let mut buf = [0u8; 64 * 1024]; // 64 KB chunks
    loop {
        match std::io::Read::read(&mut decoder, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > MAX_SNAPSHOT_SIZE {
                    return Err(SnapshotError::Invalid(format!(
                        "decompressed size exceeds {}MB limit",
                        MAX_SNAPSHOT_SIZE / (1024 * 1024)
                    )));
                }
            }
            Err(e) => return Err(e.into()),
        }
    }

    let uncompressed_size = raw.len();

    // Parse entries.
    if raw.len() < 8 {
        return Err(SnapshotError::Invalid("no entry count".into()));
    }
    let entry_count = u64::from_le_bytes(raw[..8].try_into().unwrap());
    let mut offset = 8usize;
    let mut loaded = 0u64;

    while offset < raw.len() && loaded < entry_count {
        if offset + 4 > raw.len() { break; }
        let key_len = u32::from_le_bytes(raw[offset..offset+4].try_into().unwrap()) as usize;
        offset += 4;

        if offset + key_len + 4 > raw.len() { break; }
        let key = &raw[offset..offset+key_len];
        offset += key_len;

        let val_len = u32::from_le_bytes(raw[offset..offset+4].try_into().unwrap()) as usize;
        offset += 4;

        if offset + val_len > raw.len() { break; }
        let val = &raw[offset..offset+val_len];
        offset += val_len;

        store.put(key, val).map_err(|e| SnapshotError::Storage(e.to_string()))?;
        loaded += 1;
    }

    // Verify all entries were loaded — reject truncated snapshots.
    if loaded < entry_count {
        return Err(SnapshotError::Invalid(format!(
            "truncated snapshot: expected {} entries, loaded {}",
            entry_count, loaded
        )));
    }

    store.commit_root();

    // Verify state root.
    let actual_root = store.state_root();
    if actual_root != meta.state_root {
        let hex = |h: &[u8]| -> String { h.iter().map(|b| format!("{b:02x}")).collect() };
        return Err(SnapshotError::StateRootMismatch {
            expected: hex(&meta.state_root),
            actual: hex(&actual_root),
        });
    }

    info!(
        height = meta.height,
        epoch = meta.epoch,
        entries = loaded,
        compressed = meta.compressed_size,
        uncompressed = uncompressed_size,
        "snapshot restored and verified"
    );

    Ok(SnapshotMeta {
        height: meta.height,
        epoch: meta.epoch,
        state_root: meta.state_root,
        entry_count: loaded,
        compressed_size: meta.compressed_size,
        uncompressed_size,
    })
}

/// Manages a small ring of on-disk state snapshots ("local checkpoints") so a
/// diverged node can roll back from a *local* file instead of re-downloading
/// the entire chain from a remote RPC. Each file is a full snapshot blob in the
/// same format as [`create_snapshot`] / [`restore_snapshot`], named
/// `snap-<height>.bin`. The newest `keep` are retained.
///
/// This is distinct from the consensus `checkpoint` module (which tracks
/// lightweight attestation/trusted markers, not full state).
#[derive(Clone)]
pub struct LocalSnapshots {
    dir: std::path::PathBuf,
    keep: usize,
}

impl LocalSnapshots {
    pub fn new(dir: impl Into<std::path::PathBuf>, keep: usize) -> Self {
        Self { dir: dir.into(), keep: keep.max(1) }
    }

    /// Persist `bytes` as the checkpoint for `height` (atomic publish via a
    /// temp file + rename), then prune to the newest `keep`. Best-effort.
    pub fn persist(&self, height: u64, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!("snap-{height}.bin.tmp"));
        let final_path = self.dir.join(format!("snap-{height}.bin"));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &final_path)?;
        self.prune();
        Ok(final_path)
    }

    /// All checkpoints, ascending by height.
    pub fn list(&self) -> Vec<(u64, std::path::PathBuf)> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if let Some(h) = Self::height_of(&path) {
                    out.push((h, path));
                }
            }
        }
        out.sort_by_key(|(h, _)| *h);
        out
    }

    /// The newest checkpoint at or below `height`, if any.
    pub fn newest_at_or_below(&self, height: u64) -> Option<(u64, std::path::PathBuf)> {
        self.list().into_iter().rev().find(|(h, _)| *h <= height)
    }

    fn prune(&self) {
        let all = self.list();
        if all.len() <= self.keep {
            return;
        }
        for (_, path) in &all[..all.len() - self.keep] {
            let _ = std::fs::remove_file(path);
        }
    }

    fn height_of(path: &std::path::Path) -> Option<u64> {
        path.file_name()?
            .to_str()?
            .strip_prefix("snap-")?
            .strip_suffix(".bin")?
            .parse()
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solen_storage::MemoryStore;

    #[test]
    fn snapshot_roundtrip() {
        let mut store = MemoryStore::new();
        for i in 0u32..100 {
            store.put(&i.to_le_bytes(), &(i * 3).to_le_bytes()).unwrap();
        }
        let original_root = store.state_root();

        let data = create_snapshot(&store, 42, 5).unwrap();
        let meta = read_snapshot_meta(&data).unwrap();
        assert_eq!(meta.height, 42);
        assert_eq!(meta.epoch, 5);
        assert_eq!(meta.state_root, original_root);

        // Restore into a fresh store.
        let mut restored = MemoryStore::new();
        let result = restore_snapshot(&mut restored, &data).unwrap();
        assert_eq!(result.height, 42);
        assert_eq!(result.entry_count, 100);
        assert_eq!(restored.state_root(), original_root);

        // Verify all data matches.
        for i in 0u32..100 {
            assert_eq!(
                restored.get(&i.to_le_bytes()).unwrap(),
                Some((i * 3).to_le_bytes().to_vec()),
            );
        }
    }

    #[test]
    fn empty_snapshot() {
        let store = MemoryStore::new();
        let data = create_snapshot(&store, 0, 0).unwrap();
        let mut restored = MemoryStore::new();
        let result = restore_snapshot(&mut restored, &data).unwrap();
        assert_eq!(result.entry_count, 0);
        assert_eq!(restored.state_root(), [0u8; 32]);
    }

    #[test]
    fn corrupted_snapshot_rejected() {
        let mut store = MemoryStore::new();
        store.put(b"key", b"value").unwrap();
        let mut data = create_snapshot(&store, 1, 0).unwrap();

        // Corrupt the state root in the header.
        data[24] ^= 0xFF;

        let mut restored = MemoryStore::new();
        let result = restore_snapshot(&mut restored, &data);
        assert!(matches!(result, Err(SnapshotError::StateRootMismatch { .. })));
    }

    #[test]
    fn bad_magic_rejected() {
        let result = read_snapshot_meta(b"BADXxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx");
        assert!(matches!(result, Err(SnapshotError::Invalid(_))));
    }

    #[test]
    fn local_snapshots_persist_prune_and_lookup() {
        let dir = std::env::temp_dir().join(format!("solen-localsnap-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cp = LocalSnapshots::new(&dir, 3);

        // Persist five checkpoints; only the newest 3 should survive.
        for h in [100u64, 200, 300, 400, 500] {
            cp.persist(h, format!("snapshot-at-{h}").as_bytes()).unwrap();
        }
        let heights: Vec<u64> = cp.list().into_iter().map(|(h, _)| h).collect();
        assert_eq!(heights, vec![300, 400, 500], "should keep only newest 3");

        // newest_at_or_below picks the right one.
        assert_eq!(cp.newest_at_or_below(450).map(|(h, _)| h), Some(400));
        assert_eq!(cp.newest_at_or_below(500).map(|(h, _)| h), Some(500));
        assert_eq!(cp.newest_at_or_below(250).map(|(h, _)| h), None, "all retained are above 250");

        // Bytes round-trip.
        let (_, path) = cp.newest_at_or_below(500).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"snapshot-at-500");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
