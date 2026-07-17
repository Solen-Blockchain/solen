//! stSOLEN APY sampler + endpoint.
//!
//! Periodically reads `total_pooled_solen` and `total_supply` from the
//! deployed stSOLEN contract's storage, keeps a rolling 31-day buffer of
//! samples, and exposes derived APY numbers at `/api/stsolen/apy`.
//!
//! APY model: stSOLEN's exchange rate appreciates as the pool earns
//! validator rewards. So
//!
//! ```text
//! rate(t)       = pool(t) / supply(t)
//! apy_window    = (rate_now / rate_then) ^ (year / window) - 1
//! ```
//!
//! No staking-specific math here — this is purely the price-per-stSOLEN
//! growth observed via the contract's own bookkeeping. Same pattern Lido /
//! Rocket Pool / cbETH use.
//!
//! **Address**: hardcoded per network. Mainnet is the v1.1 contract. Testnet
//! /devnet are unset; the sampler skips them.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::State;
use axum::response::Json;
use serde::Serialize;
use solen_consensus::engine::ConsensusEngine;
use tracing::{debug, warn};

/// Seconds between samples.
const SAMPLE_INTERVAL_SECS: u64 = 600; // 10 min
/// Max age of a sample before it's evicted.
const RETENTION_SECS: u64 = 31 * 24 * 60 * 60; // 31 days
/// Year length used for annualization (matches industry stETH/cbETH convention).
const SECONDS_PER_YEAR: f64 = 365.25 * 24.0 * 3600.0;

/// Mainnet stSOLEN v1.1 address (hex). If you ever redeploy, update this.
const STSOLEN_MAINNET_HEX: &str =
    "42c227f9bd58acda8a08f1d274ba61603f08cf8f194fbdd96ad10ceb943c246b";

#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub ts_ms: u64,
    pub pool: u128,
    pub supply: u128,
}

#[derive(Default)]
pub struct ApySamples {
    inner: RwLock<VecDeque<Sample>>,
}

impl ApySamples {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, s: Sample) {
        let mut buf = self.inner.write().unwrap();
        let cutoff = s.ts_ms.saturating_sub(RETENTION_SECS * 1000);
        while buf.front().map(|x| x.ts_ms < cutoff).unwrap_or(false) {
            buf.pop_front();
        }
        buf.push_back(s);
    }

    /// Most recent sample; None if buffer is empty.
    pub fn latest(&self) -> Option<Sample> {
        self.inner.read().unwrap().back().copied()
    }

    /// Oldest sample whose timestamp is ≤ `target_ts_ms`. None if no sample
    /// is that old (i.e. window not yet covered).
    pub fn at_or_before(&self, target_ts_ms: u64) -> Option<Sample> {
        let buf = self.inner.read().unwrap();
        // Buffer is roughly time-ordered; scan backwards from the end.
        for s in buf.iter().rev() {
            if s.ts_ms <= target_ts_ms {
                return Some(*s);
            }
        }
        None
    }

    pub fn count(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    pub fn oldest(&self) -> Option<Sample> {
        self.inner.read().unwrap().front().copied()
    }
}

/// Build the storage key for `cs/{contract_id}/{inner}`, matching
/// `solen-execution::state::contract_storage_key`.
fn cs_key(contract: &[u8; 32], inner: &[u8]) -> Vec<u8> {
    let mut k = b"cs/".to_vec();
    k.extend_from_slice(contract);
    k.push(b'/');
    k.extend_from_slice(inner);
    k
}

fn read_u128(store: &dyn solen_storage::StateStore, key: &[u8]) -> Option<u128> {
    match store.get(key) {
        Ok(Some(data)) if data.len() >= 16 => {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&data[..16]);
            Some(u128::from_le_bytes(buf))
        }
        _ => None,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn read_sample(engine: &ConsensusEngine, contract: &[u8; 32]) -> Option<Sample> {
    let store = engine.store();
    let guard = store.read().ok()?;
    let store: &dyn solen_storage::StateStore = &**guard;
    let pool = read_u128(store, &cs_key(contract, b"total_pooled_solen"))?;
    let supply = read_u128(store, &cs_key(contract, b"total_supply"))?;
    Some(Sample { ts_ms: now_ms(), pool, supply })
}

fn parse_addr(hex: &str) -> Option<[u8; 32]> {
    let raw = hex::decode(hex.trim_start_matches("0x")).ok()?;
    if raw.len() != 32 {
        return None;
    }
    let mut a = [0u8; 32];
    a.copy_from_slice(&raw);
    Some(a)
}

/// Spawn the periodic sampler. Reads pool + supply every `SAMPLE_INTERVAL_SECS`
/// from the deployed contract's storage and pushes a sample.
pub fn spawn_sampler(samples: Arc<ApySamples>, engine: Arc<ConsensusEngine>) {
    let contract = match parse_addr(STSOLEN_MAINNET_HEX) {
        Some(a) => a,
        None => {
            warn!("stsolen_apy: invalid mainnet address constant; sampler not started");
            return;
        }
    };

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(SAMPLE_INTERVAL_SECS));
        // First tick fires immediately so we have a sample on hand asap.
        loop {
            interval.tick().await;
            match read_sample(&engine, &contract) {
                Some(s) => {
                    debug!(pool = s.pool, supply = s.supply, "stsolen_apy sample");
                    samples.push(s);
                }
                None => {
                    debug!("stsolen_apy: contract not yet readable; will retry");
                }
            }
        }
    });
}

#[derive(Serialize)]
pub struct ApyResponse {
    /// Annualized rate over the trailing 7 days, expressed as a fraction
    /// (0.052 = 5.2 %). Null when the buffer has < 7 days of coverage.
    pub apy_7d: Option<f64>,
    /// Same, trailing 30 days.
    pub apy_30d: Option<f64>,
    /// Annualized rate from the oldest sample to now. Always present once at
    /// least two samples exist, but very noisy at small windows.
    pub since_launch: Option<f64>,
    /// Current pool (base units, decimal string).
    pub pool: String,
    /// Current total stSOLEN supply.
    pub supply: String,
    /// Number of samples in the rolling buffer.
    pub samples: usize,
    /// Wall-clock timestamp of the oldest sample (ms epoch).
    pub oldest_sample_ts_ms: u64,
}

fn annualize(rate_now: f64, rate_then: f64, window_secs: f64) -> Option<f64> {
    if window_secs <= 0.0 || rate_then <= 0.0 || !rate_now.is_finite() || !rate_then.is_finite() {
        return None;
    }
    let ratio = rate_now / rate_then;
    if !ratio.is_finite() || ratio <= 0.0 {
        return None;
    }
    let exp = SECONDS_PER_YEAR / window_secs;
    let apy = ratio.powf(exp) - 1.0;
    if apy.is_finite() {
        Some(apy)
    } else {
        None
    }
}

fn rate_f64(s: &Sample) -> f64 {
    if s.supply == 0 {
        return 0.0;
    }
    // u128 → f64 via lossy cast is fine for display math; we're not making
    // settlement decisions on this number.
    (s.pool as f64) / (s.supply as f64)
}

pub async fn get_stsolen_apy(
    State(state): State<crate::api::ApiState>,
) -> Json<ApyResponse> {
    let samples = state.stsolen_apy.clone();
    let latest = match samples.latest() {
        Some(s) => s,
        None => {
            return Json(ApyResponse {
                apy_7d: None,
                apy_30d: None,
                since_launch: None,
                pool: "0".into(),
                supply: "0".into(),
                samples: 0,
                oldest_sample_ts_ms: 0,
            });
        }
    };
    let oldest = samples.oldest();
    let rate_now = rate_f64(&latest);

    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
    let window = |days: u64| -> Option<f64> {
        let target = latest.ts_ms.checked_sub(days * DAY_MS)?;
        let earliest_ts = oldest?.ts_ms;
        if earliest_ts > target {
            // Buffer doesn't cover the window yet.
            return None;
        }
        let s = samples.at_or_before(target)?;
        let secs = (latest.ts_ms.saturating_sub(s.ts_ms) as f64) / 1000.0;
        annualize(rate_now, rate_f64(&s), secs)
    };

    let since_launch = oldest.and_then(|o| {
        let secs = (latest.ts_ms.saturating_sub(o.ts_ms) as f64) / 1000.0;
        annualize(rate_now, rate_f64(&o), secs)
    });

    Json(ApyResponse {
        apy_7d: window(7),
        apy_30d: window(30),
        since_launch,
        pool: latest.pool.to_string(),
        supply: latest.supply.to_string(),
        samples: samples.count(),
        oldest_sample_ts_ms: oldest.map(|o| o.ts_ms).unwrap_or(0),
    })
}
