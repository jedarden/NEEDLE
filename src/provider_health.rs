//! Adapter-level failure storms are infrastructure, not bead failures (N-T23).
//!
//! On 2026-09-10 the `claude-print` adapter's stream-json watchdog killed
//! 341 attempts in one day with exit code 124. Every one was booked as a
//! bead failure: failure counts climbed, beads were quarantined, and the
//! adapter selection evidence recorded a day of "task failures" for work
//! that never ran. The gate-health fingerprint detector (N-T22) already
//! answers the equivalent question for verification gates — "is one
//! fingerprint dominating recent failures across distinct beads?" — so this
//! module applies the same detector to the adapter's own failure signal:
//! exit code, stream terminal reason, API error status, timeout, signal.
//!
//! State lives per adapter at `~/.needle/state/provider-health/<hash>.json`
//! (every worker on the host shares it, and it survives restarts). When the
//! detector trips, the adapter is degraded until a verified success on it:
//!
//! - failures carrying the degraded fingerprint resolve as
//!   `infrastructure_failure`, release the bead with no failure count, and do
//!   not feed quarantine or adapter competence;
//! - workers on the adapter hold before claiming while the adapter's last
//!   degraded failure is younger than the configured cooldown, so a dead
//!   provider is not hammered with claim/release churn;
//! - `provider.degraded` / `provider.restored` events mark both edges.
//!
//! A failure with a *different* fingerprint on a degraded adapter is still
//! judged as the bead's own result.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::gate_health::VerificationRecording;
use crate::verification_fingerprint::{
    normalize_output, DetectorConfig, FingerprintTracker, VerificationFailure,
};

/// Persistent health of one adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHealthState {
    /// Adapter name (e.g. `claude-print`).
    pub adapter: String,
    /// Failure window behind the fingerprint detector, oldest first.
    #[serde(default)]
    pub window: Vec<VerificationFailure>,
    /// Whether the adapter is currently degraded.
    #[serde(default)]
    pub degraded: bool,
    /// The fingerprint that degraded it.
    #[serde(default)]
    pub degraded_fingerprint: Option<String>,
    /// Normalized failure text behind that fingerprint.
    #[serde(default)]
    pub degraded_summary: Option<String>,
    /// RFC 3339 time the adapter became degraded.
    #[serde(default)]
    pub degraded_at: Option<String>,
    /// RFC 3339 time of the most recent failure carrying the degraded
    /// fingerprint — what the claim hold measures from.
    #[serde(default)]
    pub last_degraded_failure_at: Option<String>,
}

impl ProviderHealthState {
    fn new(adapter: &str) -> Self {
        Self {
            adapter: adapter.to_string(),
            window: Vec::new(),
            degraded: false,
            degraded_fingerprint: None,
            degraded_summary: None,
            degraded_at: None,
            last_degraded_failure_at: None,
        }
    }

    /// Seconds since the adapter's last degraded failure, if it is degraded.
    pub fn secs_since_last_degraded_failure(&self) -> Option<u64> {
        if !self.degraded {
            return None;
        }
        let at = self
            .last_degraded_failure_at
            .as_deref()
            .or(self.degraded_at.as_deref())?;
        let parsed = chrono::DateTime::parse_from_rfc3339(at).ok()?;
        let elapsed = chrono::Utc::now().signed_duration_since(parsed);
        Some(elapsed.num_seconds().max(0) as u64)
    }

    /// Seconds the adapter has been degraded, if it is.
    pub fn degraded_for_secs(&self) -> Option<u64> {
        if !self.degraded {
            return None;
        }
        let at = self.degraded_at.as_deref()?;
        let parsed = chrono::DateTime::parse_from_rfc3339(at).ok()?;
        Some(
            chrono::Utc::now()
                .signed_duration_since(parsed)
                .num_seconds()
                .max(0) as u64,
        )
    }
}

/// Where adapter health files live.
fn state_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(".needle").join("state").join("provider-health")
}

/// Stable file name for an adapter.
pub fn state_file_path(adapter: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(adapter.as_bytes());
    let digest = hasher.finalize();
    let id: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    state_dir().join(format!("{id}.json"))
}

/// Load an adapter's state, `None` when it has never failed.
pub fn load_state(adapter: &str) -> Result<Option<ProviderHealthState>> {
    let path = state_file_path(adapter);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    match serde_json::from_str::<ProviderHealthState>(&text) {
        Ok(state) => Ok(Some(state)),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "provider health state unreadable — starting fresh"
            );
            Ok(None)
        }
    }
}

fn save_state(state: &ProviderHealthState) -> Result<()> {
    let path = state_file_path(&state.adapter);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

/// Record one non-gate failure of `adapter` on `bead` and decide what it
/// means. `reason` is the short failure signal (`exit_code:124`,
/// `terminal_reason=api_error api_error_status=503`, `timeout`, `signal:9`).
pub fn record_adapter_failure(
    adapter: &str,
    bead: &str,
    reason: &str,
    config: &DetectorConfig,
) -> Result<VerificationRecording> {
    let now = chrono::Utc::now();
    // The fingerprint hashes the gate name raw and the output *normalized*,
    // and normalization folds numbers — `exit_code:124` and `exit_code:1`
    // would collide. The reason therefore rides in the gate half of the hash
    // as well, so distinct exit codes and statuses stay distinct.
    let failure = VerificationFailure::new(now, bead, &format!("{adapter}::{reason}"), reason);
    let summary = normalize_output(reason);

    let mut state = load_state(adapter)?.unwrap_or_else(|| ProviderHealthState::new(adapter));
    let degraded_for = state
        .degraded_fingerprint
        .clone()
        .filter(|_| state.degraded);
    let mut tracker = FingerprintTracker::new(state.window.clone(), *config);
    let outcome = tracker.record(failure);
    state.window = tracker.window();

    let recording = match degraded_for {
        Some(degraded) if degraded == outcome.fingerprint => {
            state.last_degraded_failure_at = Some(now.to_rfc3339());
            VerificationRecording::DegradedForThisFingerprint {
                fingerprint: outcome.fingerprint,
            }
        }
        Some(degraded) => VerificationRecording::DegradedForOther {
            fingerprint: outcome.fingerprint,
            degraded,
        },
        None => match outcome.decision {
            crate::verification_fingerprint::Decision::Tripped {
                fingerprint,
                failures,
                distinct_beads,
            } => {
                state.degraded = true;
                state.degraded_fingerprint = Some(fingerprint.clone());
                state.degraded_summary = Some(summary.clone());
                state.degraded_at = Some(now.to_rfc3339());
                state.last_degraded_failure_at = Some(now.to_rfc3339());
                VerificationRecording::Tripped {
                    fingerprint,
                    failures,
                    distinct_beads,
                    summary,
                }
            }
            crate::verification_fingerprint::Decision::Window {
                failures,
                distinct_beads,
            } => VerificationRecording::Window {
                fingerprint: outcome.fingerprint,
                failures,
                distinct_beads,
            },
        },
    };

    save_state(&state)?;
    Ok(recording)
}

/// A verified success on `adapter` lifts its degradation and empties the
/// failure window. Returns the prior state when the adapter *was* degraded,
/// so the caller can emit the restoration.
pub fn record_adapter_success(adapter: &str) -> Result<Option<ProviderHealthState>> {
    let Some(state) = load_state(adapter)? else {
        return Ok(None);
    };
    let was_degraded = state.degraded;
    let path = state_file_path(adapter);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(if was_degraded { Some(state) } else { None })
}

/// The adapter's state if it is currently degraded.
pub fn degraded_state(adapter: &str) -> Result<Option<ProviderHealthState>> {
    Ok(load_state(adapter)?.filter(|s| s.degraded))
}

/// Remove an adapter's state entirely (operator reset / tests).
pub fn clear_state(adapter: &str) -> Result<()> {
    let path = state_file_path(adapter);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

/// Human-readable listing of degraded adapters under `dir`-less default
/// state (for `needle doctor`/`status`): every state file that is degraded.
pub fn degraded_adapters() -> Vec<ProviderHealthState> {
    let Ok(entries) = std::fs::read_dir(state_dir()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path: &Path = &entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(state) = serde_json::from_str::<ProviderHealthState>(&text) {
                if state.degraded {
                    out.push(state);
                }
            }
        }
    }
    out.sort_by(|a, b| a.adapter.cmp(&b.adapter));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests share the real `$HOME`-derived state directory, so each uses a
    /// unique adapter name and cleans up after itself.
    fn unique_adapter(tag: &str) -> String {
        format!(
            "test-adapter-{tag}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        )
    }

    fn quick_config() -> DetectorConfig {
        DetectorConfig {
            window: std::time::Duration::from_secs(3600),
            window_max_failures: 20,
            min_window_failures: 4,
            trip_ratio: 0.75,
            min_distinct_beads: 3,
        }
    }

    #[test]
    fn storm_across_beads_trips_and_later_same_failures_are_infra() {
        let adapter = unique_adapter("storm");
        let config = quick_config();
        let mut last = None;
        for n in 0..4 {
            last = Some(
                record_adapter_failure(&adapter, &format!("nd-{n}"), "exit_code:124", &config)
                    .unwrap(),
            );
        }
        let last = last.unwrap();
        assert!(
            matches!(last, VerificationRecording::Tripped { .. }),
            "{last:?}"
        );
        assert!(last.is_infra());
        let state = degraded_state(&adapter).unwrap().expect("degraded");
        assert_eq!(
            state.degraded_fingerprint.as_deref(),
            Some(last.fingerprint())
        );
        assert!(state.secs_since_last_degraded_failure().is_some());

        // Same fingerprint again: infra, and the hold clock restarts.
        let again = record_adapter_failure(&adapter, "nd-9", "exit_code:124", &config).unwrap();
        assert!(matches!(
            again,
            VerificationRecording::DegradedForThisFingerprint { .. }
        ));
        // A different failure on the degraded adapter is still the bead's own.
        let other = record_adapter_failure(&adapter, "nd-10", "exit_code:1", &config).unwrap();
        assert!(matches!(
            other,
            VerificationRecording::DegradedForOther { .. }
        ));
        assert!(!other.is_infra());

        // A verified success restores the adapter and reports it was degraded.
        let prior = record_adapter_success(&adapter).unwrap();
        assert!(prior.is_some_and(|s| s.degraded));
        assert!(degraded_state(&adapter).unwrap().is_none());
        clear_state(&adapter).unwrap();
    }

    #[test]
    fn one_bead_failing_repeatedly_does_not_trip() {
        let adapter = unique_adapter("single");
        let config = quick_config();
        let mut last = None;
        for _ in 0..6 {
            last =
                Some(record_adapter_failure(&adapter, "nd-1", "exit_code:124", &config).unwrap());
        }
        assert!(matches!(
            last.unwrap(),
            VerificationRecording::Window { .. }
        ));
        assert!(degraded_state(&adapter).unwrap().is_none());
        assert!(record_adapter_success(&adapter).unwrap().is_none());
        clear_state(&adapter).unwrap();
    }

    #[test]
    fn mixed_fingerprints_never_trip() {
        let adapter = unique_adapter("mixed");
        let config = quick_config();
        for n in 0..8 {
            let reason = if n % 2 == 0 {
                "exit_code:1"
            } else {
                "exit_code:2"
            };
            record_adapter_failure(&adapter, &format!("nd-{n}"), reason, &config).unwrap();
        }
        assert!(degraded_state(&adapter).unwrap().is_none());
        clear_state(&adapter).unwrap();
    }
}
