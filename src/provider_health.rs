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
//!
//! # Gateway keying (N-T51)
//!
//! Adapters that talk through one provider share that provider's fate, so
//! when the caller passes the adapter's configured provider the state is
//! keyed by it instead: a storm on `claude-code-glm-5.3` degrades
//! `opencode-glm-5.3-flash` behind the same `zai-proxy` gateway, a verified
//! success on either restores the group, and the fingerprint hashes the key
//! so the same failure signal matches across the group. Adapters that
//! declare no provider — and callers that have not turned provider keying on
//! (`WorkspaceHealthConfig::provider_keyed_health`) — pass no provider and
//! keep the per-adapter keying above, unchanged. Provider-keyed state files
//! record the adapter membership that contributed to them, and the first
//! provider-keyed write adopts any adapter-keyed file left by an earlier
//! version, so an active degradation survives the upgrade.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::gate_health::VerificationRecording;
use crate::verification_fingerprint::{
    normalize_output, DetectorConfig, FingerprintTracker, VerificationFailure,
};

/// Persistent health of one adapter — or, with gateway keying, of the
/// provider a group of adapters shares (N-T51).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHealthState {
    /// Adapter name (e.g. `claude-print`). On provider-keyed state, the
    /// adapter whose failures first created the state.
    pub adapter: String,
    /// The health key this state is filed under (N-T51): the provider the
    /// adapter group shares, or the adapter's own name when it declares
    /// none. Empty on state written before N-T51, which is adapter-keyed.
    #[serde(default)]
    pub provider: String,
    /// Adapters observed behind this key, sorted and deduplicated. Empty on
    /// state written before N-T51.
    #[serde(default)]
    pub adapters: Vec<String>,
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
    fn new(key: &str, adapter: &str) -> Self {
        Self {
            adapter: adapter.to_string(),
            provider: key.to_string(),
            adapters: vec![adapter.to_string()],
            window: Vec::new(),
            degraded: false,
            degraded_fingerprint: None,
            degraded_summary: None,
            degraded_at: None,
            last_degraded_failure_at: None,
        }
    }

    /// The key this state is filed under: the provider when set, else the
    /// adapter name (state written before N-T51, or an adapter with none).
    pub fn health_key(&self) -> &str {
        if self.provider.is_empty() {
            &self.adapter
        } else {
            &self.provider
        }
    }

    /// Every adapter the state knows to share this health: the recorded
    /// membership, or just the adapter on state written before N-T51.
    pub fn affected_adapters(&self) -> Vec<String> {
        if self.adapters.is_empty() {
            vec![self.adapter.clone()]
        } else {
            self.adapters.clone()
        }
    }

    /// Record `adapter` as part of this health group. Returns whether the
    /// membership grew, so callers only persist when it did.
    fn note_adapter(&mut self, adapter: &str) -> bool {
        if self.adapters.iter().any(|a| a == adapter) {
            return false;
        }
        self.adapters.push(adapter.to_string());
        self.adapters.sort();
        true
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

/// The health key for an adapter (N-T51): the provider its configuration
/// names, falling back to the adapter's own name. Callers pass the provider
/// only when provider keying is enabled
/// (`WorkspaceHealthConfig::provider_keyed_health`), so keying stays per
/// adapter — the shipped N-T23 behavior — until the flag turns it on.
pub fn resolve_key<'a>(provider: Option<&'a str>, adapter: &'a str) -> &'a str {
    provider.unwrap_or(adapter)
}

/// Where adapter health files live: the state root's `provider-health`
/// (`~/.needle/state/provider-health` by default; ADR-030 decision 5).
fn state_dir() -> PathBuf {
    crate::state_dir::provider_health_dir()
}

/// Stable file name for a health key — an adapter before N-T51, the
/// provider once keying is on.
pub fn state_file_path(key: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let id: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    state_dir().join(format!("{id}.json"))
}

/// Read one state file, `None` when it does not exist or is unreadable.
fn read_state_file(path: &Path) -> Result<Option<ProviderHealthState>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
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

/// Load an adapter's state under its health key, migrating an adapter-keyed
/// file written before N-T51 (or before keying was enabled) into the
/// provider-keyed file without losing an active degradation: the legacy
/// window and degradation merge into the keyed state, the keyed state is
/// persisted, and the legacy file is removed. Read paths never write here —
/// only the recording paths call this.
fn load_keyed(adapter: &str, provider: Option<&str>) -> Result<Option<ProviderHealthState>> {
    let key = resolve_key(provider, adapter);
    let key_path = state_file_path(key);
    let legacy_path = state_file_path(adapter);
    let legacy = if key == adapter {
        // Unkeyed: the adapter-keyed file *is* the keyed file.
        None
    } else {
        read_state_file(&legacy_path)?
    };

    match (read_state_file(&key_path)?, legacy) {
        (Some(mut state), Some(legacy)) => {
            merge_legacy(&mut state, legacy, adapter, key);
            write_state_file(&key_path, &state)?;
            remove_state_file(&legacy_path)?;
            Ok(Some(state))
        }
        (None, Some(legacy)) => {
            let mut migrated = legacy;
            migrated.provider = key.to_string();
            migrated.note_adapter(adapter);
            write_state_file(&key_path, &migrated)?;
            remove_state_file(&legacy_path)?;
            Ok(Some(migrated))
        }
        (Some(mut state), None) => {
            if state.note_adapter(adapter) {
                write_state_file(&key_path, &state)?;
            }
            Ok(Some(state))
        }
        (None, None) => Ok(None),
    }
}

/// Fold a legacy adapter-keyed state into the provider-keyed one: the
/// failure windows union (the detector re-prunes and re-evaluates on the
/// next record) and an active degradation on either side survives, so the
/// upgrade cannot silently un-degrade a provider mid-storm.
fn merge_legacy(
    state: &mut ProviderHealthState,
    legacy: ProviderHealthState,
    adapter: &str,
    key: &str,
) {
    state.provider = key.to_string();
    state.note_adapter(adapter);
    for seen in legacy.adapters {
        state.adapters.push(seen);
    }
    state.adapters.sort();
    state.adapters.dedup();
    let mut window = std::mem::take(&mut state.window);
    window.extend(legacy.window);
    window.sort_by_key(|f| f.at);
    state.window = window;
    if legacy.degraded && !state.degraded {
        state.degraded = legacy.degraded;
        state.degraded_fingerprint = legacy.degraded_fingerprint;
        state.degraded_summary = legacy.degraded_summary;
        state.degraded_at = legacy.degraded_at;
        state.last_degraded_failure_at = legacy.last_degraded_failure_at;
    }
}

fn write_state_file(path: &Path, state: &ProviderHealthState) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}

fn remove_state_file(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

/// Record one non-gate failure of `adapter` on `bead` and decide what it
/// means. `provider` is the adapter's configured provider when gateway
/// keying is enabled, `None` otherwise (N-T51). `reason` is the short
/// failure signal (`exit_code:124`,
/// `terminal_reason=api_error api_error_status=503`, `timeout`, `signal:9`).
pub fn record_adapter_failure(
    adapter: &str,
    provider: Option<&str>,
    bead: &str,
    reason: &str,
    config: &DetectorConfig,
) -> Result<VerificationRecording> {
    let now = chrono::Utc::now();
    let key = resolve_key(provider, adapter);
    // The fingerprint hashes the gate name raw and the output *normalized*,
    // and normalization folds numbers — `exit_code:124` and `exit_code:1`
    // would collide. The reason therefore rides in the gate half of the hash
    // as well, so distinct exit codes and statuses stay distinct. The gate
    // half carries the health key, not the adapter name, so behind one
    // provider the same failure signal is one fingerprint across every
    // adapter in the group (N-T51); unkeyed adapters hash their own name and
    // see exactly the fingerprints they always did.
    let failure = VerificationFailure::new(now, bead, &format!("{key}::{reason}"), reason);
    let summary = normalize_output(reason);

    let mut state =
        load_keyed(adapter, provider)?.unwrap_or_else(|| ProviderHealthState::new(key, adapter));
    state.note_adapter(adapter);
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

    write_state_file(&state_file_path(state.health_key()), &state)?;
    Ok(recording)
}

/// A verified success on `adapter` lifts the degradation of its whole
/// health group and empties the failure window (N-T51). Returns the prior
/// state when the group *was* degraded, so the caller can emit the
/// restoration.
pub fn record_adapter_success(
    adapter: &str,
    provider: Option<&str>,
) -> Result<Option<ProviderHealthState>> {
    let Some(state) = load_keyed(adapter, provider)? else {
        return Ok(None);
    };
    let was_degraded = state.degraded;
    let key_path = state_file_path(state.health_key());
    if key_path.exists() {
        std::fs::remove_file(&key_path)
            .with_context(|| format!("failed to remove {}", key_path.display()))?;
    }
    Ok(if was_degraded { Some(state) } else { None })
}

/// The adapter's state if its health group is currently degraded. Read-only:
/// unlike the recording paths this never migrates a legacy file, but it
/// falls back to one so a degradation that predates the upgrade still holds
/// the adapter that tripped it until its next record adopts the keyed file.
pub fn degraded_state(
    adapter: &str,
    provider: Option<&str>,
) -> Result<Option<ProviderHealthState>> {
    let key = resolve_key(provider, adapter);
    let state = match read_state_file(&state_file_path(key))? {
        Some(state) => Some(state),
        None if key != adapter => read_state_file(&state_file_path(adapter))?,
        None => None,
    };
    Ok(state.filter(|s| s.degraded))
}

/// Remove an adapter's state entirely — its health group's keyed file and
/// any legacy adapter-keyed file (operator reset / tests).
pub fn clear_state(adapter: &str, provider: Option<&str>) -> Result<()> {
    let key = resolve_key(provider, adapter);
    for path in [state_file_path(key), state_file_path(adapter)] {
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
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

/// Every adapter whose health is currently degraded, expanded to the whole
/// provider group (N-T51). `known` carries each configured adapter's name
/// and its provider (`None` when it declares none or keying is off): a
/// known adapter joins when its health key matches a degraded state's key,
/// so routing evidence treats every adapter behind one degraded provider as
/// unfrozen at once. A degraded state also reports the membership it
/// recorded, so adapters the caller does not know about are not lost.
pub fn expand_degraded_adapters(
    states: &[ProviderHealthState],
    known: impl IntoIterator<Item = (String, Option<String>)>,
) -> Vec<String> {
    let known: Vec<(String, Option<String>)> = known.into_iter().collect();
    // A legacy adapter-keyed file has no provider field. Resolve that old
    // adapter name against the current roster before matching keys, so an
    // active degradation expands across its gateway even before the first
    // provider-keyed write performs the on-disk migration.
    let degraded_keys: std::collections::BTreeSet<String> = states
        .iter()
        .map(|state| {
            if state.provider.is_empty() {
                known
                    .iter()
                    .find(|(name, _)| name == &state.adapter)
                    .and_then(|(_, provider)| provider.as_deref())
                    .unwrap_or_else(|| state.health_key())
                    .to_string()
            } else {
                state.health_key().to_string()
            }
        })
        .collect();
    let mut out = std::collections::BTreeSet::new();
    for (name, provider) in &known {
        if degraded_keys.contains(resolve_key(provider.as_deref(), name)) {
            out.insert(name.clone());
        }
    }
    for state in states {
        out.extend(state.affected_adapters());
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Pin the HOME-derived provider-health state beneath a per-test root.
    /// The environment lock makes this safe alongside every other unit test
    /// that swaps HOME process-globally.
    fn isolated_home() -> (crate::util::test_env::EnvGuard, TempDir) {
        let env_guard = crate::util::test_env::isolate_env();
        let home = TempDir::new().unwrap();
        std::env::set_var("HOME", home.path());
        (env_guard, home)
    }

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
        let (_env_guard, _home) = isolated_home();
        let adapter = unique_adapter("storm");
        let config = quick_config();
        let mut last = None;
        for n in 0..4 {
            last = Some(
                record_adapter_failure(
                    &adapter,
                    None,
                    &format!("nd-{n}"),
                    "exit_code:124",
                    &config,
                )
                .unwrap(),
            );
        }
        let last = last.unwrap();
        assert!(
            matches!(last, VerificationRecording::Tripped { .. }),
            "{last:?}"
        );
        assert!(last.is_infra());
        let state = degraded_state(&adapter, None).unwrap().expect("degraded");
        assert_eq!(state.health_key(), adapter);
        assert_eq!(
            state.degraded_fingerprint.as_deref(),
            Some(last.fingerprint())
        );
        assert!(state.secs_since_last_degraded_failure().is_some());

        // Same fingerprint again: infra, and the hold clock restarts.
        let again =
            record_adapter_failure(&adapter, None, "nd-9", "exit_code:124", &config).unwrap();
        assert!(matches!(
            again,
            VerificationRecording::DegradedForThisFingerprint { .. }
        ));
        // A different failure on the degraded adapter is still the bead's own.
        let other =
            record_adapter_failure(&adapter, None, "nd-10", "exit_code:1", &config).unwrap();
        assert!(matches!(
            other,
            VerificationRecording::DegradedForOther { .. }
        ));
        assert!(!other.is_infra());

        // A verified success restores the adapter and reports it was degraded.
        let prior = record_adapter_success(&adapter, None).unwrap();
        assert!(prior.is_some_and(|s| s.degraded));
        assert!(degraded_state(&adapter, None).unwrap().is_none());
        clear_state(&adapter, None).unwrap();
    }

    #[test]
    fn one_bead_failing_repeatedly_does_not_trip() {
        let (_env_guard, _home) = isolated_home();
        let adapter = unique_adapter("single");
        let config = quick_config();
        let mut last = None;
        for _ in 0..6 {
            last = Some(
                record_adapter_failure(&adapter, None, "nd-1", "exit_code:124", &config).unwrap(),
            );
        }
        assert!(matches!(
            last.unwrap(),
            VerificationRecording::Window { .. }
        ));
        assert!(degraded_state(&adapter, None).unwrap().is_none());
        assert!(record_adapter_success(&adapter, None).unwrap().is_none());
        clear_state(&adapter, None).unwrap();
    }

    #[test]
    fn mixed_fingerprints_never_trip() {
        let (_env_guard, _home) = isolated_home();
        let adapter = unique_adapter("mixed");
        let config = quick_config();
        for n in 0..8 {
            let reason = if n % 2 == 0 {
                "exit_code:1"
            } else {
                "exit_code:2"
            };
            record_adapter_failure(&adapter, None, &format!("nd-{n}"), reason, &config).unwrap();
        }
        assert!(degraded_state(&adapter, None).unwrap().is_none());
        clear_state(&adapter, None).unwrap();
    }
}
