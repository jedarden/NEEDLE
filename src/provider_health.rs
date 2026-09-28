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
//! exit code, stream terminal reason, API error status, timeout, signal. A
//! generic non-zero exit is included too: some agent CLIs use exit 1 for
//! transport or invocation failures, and the cross-bead threshold is what
//! distinguishes that shared failure from one bead's ordinary failed attempt.
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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::gate_health::VerificationRecording;
use crate::verification_fingerprint::{
    normalize_output, DetectorConfig, FingerprintTracker, VerificationFailure,
};

/// Per-attempt observations extracted from Claude's stream-json output.
///
/// Counts are deliberately categorical: the stream is inspected while it is
/// in flight, but no provider error body is retained in the attempt ledger or
/// the provider-health state file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderErrorMetrics {
    /// Number of provider-error observations in this attempt. A record that
    /// contains both an API error and an HTTP status is counted once.
    pub provider_errors: u32,
    /// Stable error classes and their counts. Keys are `api_error`, `retry`,
    /// and `http_429`, `http_502`, `http_503`, or `http_529`.
    pub provider_error_classes: BTreeMap<String, u32>,
    /// Longest timestamp gap between consecutive assistant records.
    pub max_response_gap_ms: Option<u64>,
    /// Internal marker used to avoid treating arbitrary JSON adapters as a
    /// Claude stream. It is never serialized or persisted.
    #[serde(skip)]
    saw_claude_record: bool,
}

impl ProviderErrorMetrics {
    /// Whether this summary came from a Claude stream record.
    pub fn saw_claude_record(&self) -> bool {
        self.saw_claude_record
    }

    /// Whether this attempt has any provider signal worth emitting.
    pub fn has_signal(&self) -> bool {
        self.provider_errors > 0 || self.max_response_gap_ms.is_some()
    }
}

/// Incremental Claude stream analyzer used by both the output transform and
/// attempt resolution. The fallback timestamp is the transform's read time;
/// replayed records use their own timestamp fields when present.
#[derive(Debug, Clone, Default)]
pub struct ProviderErrorTracker {
    metrics: ProviderErrorMetrics,
    last_assistant_ts: Option<f64>,
}

impl ProviderErrorTracker {
    /// Observe one raw Claude JSON record.
    pub fn observe_line(&mut self, line: &str, fallback_ts: f64) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        self.observe_value(&value, fallback_ts);
    }

    /// Observe one already-parsed record. This is public so transform tests
    /// can replay fixtures without going through a child process.
    pub fn observe_value(&mut self, value: &Value, fallback_ts: f64) {
        let record_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let lower_type = record_type.to_ascii_lowercase();
        let is_claude_record = matches!(
            record_type,
            "system" | "assistant" | "user" | "result" | "error" | "api_error"
        ) || record_type == "stream_event"
            || record_type == "tool_progress";
        if is_claude_record {
            self.metrics.saw_claude_record = true;
        }

        let timestamp = event_timestamp(value).unwrap_or(fallback_ts);
        let assistant_record = record_type == "assistant"
            || (record_type == "stream_event"
                && value
                    .get("event")
                    .and_then(|event| event.get("type"))
                    .and_then(Value::as_str)
                    == Some("message_start")
                && value
                    .get("event")
                    .and_then(|event| event.get("message"))
                    .and_then(|message| message.get("role"))
                    .and_then(Value::as_str)
                    == Some("assistant"));
        if assistant_record {
            if let Some(previous) = self.last_assistant_ts {
                let gap_ms = ((timestamp - previous).max(0.0) * 1000.0).round() as u64;
                self.metrics.max_response_gap_ms =
                    Some(self.metrics.max_response_gap_ms.unwrap_or(0).max(gap_ms));
            }
            self.last_assistant_ts = Some(timestamp);
        }

        let texts = string_values(value);
        let has_api_error = record_type == "api_error"
            || lower_type == "api_error"
            || value
                .get("subtype")
                .and_then(Value::as_str)
                .is_some_and(|subtype| subtype.eq_ignore_ascii_case("api_error"))
            || value
                .get("terminal_reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| reason.eq_ignore_ascii_case("api_error"));
        let has_api_error = has_api_error
            || texts
                .iter()
                .any(|text| text.eq_ignore_ascii_case("api_error"));
        let mut statuses = Vec::new();
        for status in [429, 502, 503, 529] {
            if value_has_http_status(value, status)
                || texts.iter().any(|text| contains_status(text, status))
            {
                statuses.push(status);
            }
        }
        let retry_notice = lower_type.contains("retry")
            || texts.iter().any(|text| {
                let lower = text.to_ascii_lowercase();
                lower.contains("retry") || lower.contains("rate limit; retry")
            });

        if has_api_error || !statuses.is_empty() || retry_notice {
            self.metrics.provider_errors = self.metrics.provider_errors.saturating_add(1);
        }
        if has_api_error {
            self.bump_class("api_error");
        }
        for status in statuses {
            self.bump_class(&format!("http_{status}"));
        }
        if retry_notice {
            self.bump_class("retry");
        }
    }

    /// Finish the tracker and return its categorical summary.
    pub fn finish(&self) -> ProviderErrorMetrics {
        self.metrics.clone()
    }

    fn bump_class(&mut self, class: &str) {
        *self
            .metrics
            .provider_error_classes
            .entry(class.to_string())
            .or_default() += 1;
    }
}

/// Analyze an entire Claude stream. Explicit timestamps are preferred; a
/// millisecond-spaced fallback keeps timestamp-free replay deterministic and
/// prevents parsing speed from inventing a long response gap.
pub fn analyze_claude_stream(stream: &str) -> ProviderErrorMetrics {
    let mut tracker = ProviderErrorTracker::default();
    for (index, line) in stream.lines().enumerate() {
        tracker.observe_line(line, index as f64 / 1000.0);
    }
    tracker.finish()
}

/// Extract a record timestamp in epoch seconds from the common Claude replay
/// spellings. Numeric timestamps larger than epoch seconds are interpreted as
/// milliseconds or microseconds; RFC3339 strings are also accepted.
pub fn event_timestamp(value: &Value) -> Option<f64> {
    for key in ["timestamp", "ts", "event_time", "created_at", "time"] {
        if let Some(timestamp) = value.get(key).and_then(timestamp_value) {
            return Some(timestamp);
        }
    }
    if let Some(event) = value.get("event") {
        if let Some(timestamp) = event_timestamp(event) {
            return Some(timestamp);
        }
    }
    if let Some(message) = value.get("message") {
        if let Some(timestamp) = event_timestamp(message) {
            return Some(timestamp);
        }
    }
    None
}

fn timestamp_value(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => {
            if let Ok(number) = text.parse::<f64>() {
                Some(number)
            } else {
                DateTime::parse_from_rfc3339(text)
                    .ok()
                    .map(|timestamp| timestamp.timestamp_millis() as f64 / 1000.0)
            }
        }
        _ => None,
    }?;
    if number.abs() > 100_000_000_000_000.0 {
        Some(number / 1_000_000.0)
    } else if number.abs() > 100_000_000_000.0 {
        Some(number / 1_000.0)
    } else {
        Some(number)
    }
}

fn string_values(value: &Value) -> Vec<String> {
    let mut values = Vec::new();
    collect_string_values(value, &mut values);
    values
}

fn collect_string_values(value: &Value, values: &mut Vec<String>) {
    match value {
        Value::String(text) => values.push(text.clone()),
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_string_values(item, values)),
        Value::Object(fields) => fields
            .values()
            .for_each(|item| collect_string_values(item, values)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn value_has_http_status(value: &Value, status: u16) -> bool {
    ["status", "status_code", "http_status", "api_error_status"]
        .iter()
        .filter_map(|key| value.get(*key))
        .any(|candidate| {
            candidate.as_u64() == Some(u64::from(status))
                || candidate.as_str() == Some(&status.to_string())
        })
}

fn contains_status(text: &str, status: u16) -> bool {
    let needle = status.to_string();
    text.match_indices(&needle).any(|(index, _)| {
        let before = text[..index].chars().next_back();
        let after = text[index + needle.len()..].chars().next();
        !before.is_some_and(|character| character.is_ascii_digit())
            && !after.is_some_and(|character| character.is_ascii_digit())
    })
}

/// Sliding-window provider error-share configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProviderErrorConfig {
    pub window: std::time::Duration,
    pub degraded_threshold: f64,
    pub restored_threshold: f64,
    pub min_attempts: usize,
}

impl Default for ProviderErrorConfig {
    fn default() -> Self {
        Self {
            window: std::time::Duration::from_secs(30 * 60),
            degraded_threshold: 0.5,
            restored_threshold: 0.2,
            min_attempts: 5,
        }
    }
}

/// One compact, body-free attempt observation retained by provider health.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderErrorAttempt {
    pub at: DateTime<Utc>,
    pub provider_errors: u32,
    #[serde(default)]
    pub classes: BTreeMap<String, u32>,
}

/// Persisted health state for one `(adapter, provider)` stream-error window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderErrorHealthState {
    pub adapter: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub attempts: Vec<ProviderErrorAttempt>,
    #[serde(default)]
    pub error_share: f64,
    #[serde(default)]
    pub error_attempts: usize,
    #[serde(default)]
    pub degraded: bool,
    #[serde(default)]
    pub degraded_at: Option<String>,
}

/// Edge emitted by [`record_provider_attempt`].
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderHealthTransition {
    None { degraded: bool, error_share: f64 },
    Degraded { error_share: f64, attempts: usize },
    Restored { error_share: f64, attempts: usize },
}

impl ProviderErrorHealthState {
    fn new(adapter: &str, provider: Option<&str>) -> Self {
        Self {
            adapter: adapter.to_string(),
            provider: provider.map(str::to_string),
            attempts: Vec::new(),
            error_share: 0.0,
            error_attempts: 0,
            degraded: false,
            degraded_at: None,
        }
    }
}

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

fn provider_error_state_file_path(adapter: &str, provider: Option<&str>) -> PathBuf {
    let provider = provider.unwrap_or("");
    state_file_path(&format!("stream-errors\u{1f}{adapter}\u{1f}{provider}"))
}

fn read_provider_error_state(path: &Path) -> Result<Option<ProviderErrorHealthState>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    match serde_json::from_str::<ProviderErrorHealthState>(&text) {
        Ok(state) => Ok(Some(state)),
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "provider stream health state unreadable — starting fresh"
            );
            Ok(None)
        }
    }
}

fn write_provider_error_state(path: &Path, state: &ProviderErrorHealthState) -> Result<()> {
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

/// Record one Claude-stream attempt and update the provider's sliding error
/// share. The denominator is attempts with a recognized Claude record; an
/// attempt with several error records still contributes one errored attempt.
pub fn record_provider_attempt(
    adapter: &str,
    provider: Option<&str>,
    metrics: &ProviderErrorMetrics,
    config: ProviderErrorConfig,
) -> Result<ProviderHealthTransition> {
    let now = Utc::now();
    let path = provider_error_state_file_path(adapter, provider);
    let mut state = read_provider_error_state(&path)?
        .unwrap_or_else(|| ProviderErrorHealthState::new(adapter, provider));
    let cutoff = now - chrono::Duration::from_std(config.window).unwrap_or(chrono::Duration::MAX);
    state.attempts.retain(|attempt| attempt.at >= cutoff);
    state.attempts.push(ProviderErrorAttempt {
        at: now,
        provider_errors: metrics.provider_errors,
        classes: metrics.provider_error_classes.clone(),
    });
    state.error_attempts = state
        .attempts
        .iter()
        .filter(|attempt| attempt.provider_errors > 0)
        .count();
    state.error_share = if state.attempts.is_empty() {
        0.0
    } else {
        state.error_attempts as f64 / state.attempts.len() as f64
    };

    let transition = if !state.degraded
        && state.attempts.len() >= config.min_attempts
        && state.error_share > config.degraded_threshold
    {
        state.degraded = true;
        state.degraded_at = Some(now.to_rfc3339());
        ProviderHealthTransition::Degraded {
            error_share: state.error_share,
            attempts: state.attempts.len(),
        }
    } else if state.degraded
        && state.attempts.len() >= config.min_attempts
        && state.error_share < config.restored_threshold
    {
        state.degraded = false;
        state.degraded_at = None;
        ProviderHealthTransition::Restored {
            error_share: state.error_share,
            attempts: state.attempts.len(),
        }
    } else {
        ProviderHealthTransition::None {
            degraded: state.degraded,
            error_share: state.error_share,
        }
    };
    write_provider_error_state(&path, &state)?;
    Ok(transition)
}

fn provider_error_state_as_health(state: ProviderErrorHealthState) -> ProviderHealthState {
    ProviderHealthState {
        adapter: state.adapter,
        provider: state.provider.unwrap_or_default(),
        adapters: vec![],
        window: vec![],
        degraded: state.degraded,
        degraded_fingerprint: Some("provider_error_share".to_string()),
        degraded_summary: Some(format!("error_share={:.3}", state.error_share)),
        degraded_at: state.degraded_at,
        last_degraded_failure_at: None,
    }
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
/// failure signal (`exit_code:1`, `exit_code:124`,
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
    if let Some(state) =
        read_provider_error_state(&provider_error_state_file_path(adapter, provider))?
            .filter(|state| state.degraded)
    {
        return Ok(Some(provider_error_state_as_health(state)));
    }
    let key = resolve_key(provider, adapter);
    let state = match read_state_file(&state_file_path(key))? {
        Some(state) => Some(state),
        None if key != adapter => read_state_file(&state_file_path(adapter))?,
        None => None,
    };
    Ok(state.filter(|s| s.degraded))
}

/// Whether the adapter's provider/health group is currently degraded.
///
/// This is intentionally read-only: selection and attempt provenance must be
/// able to observe the live state without migrating or otherwise changing it.
/// Both stream-derived provider-error state and the adapter failure detector
/// are covered by [`degraded_state`].
pub fn is_degraded(adapter: &str, provider: Option<&str>) -> Result<bool> {
    degraded_state(adapter, provider).map(|state| state.is_some())
}

/// Remove an adapter's state entirely — its health group's keyed file and
/// any legacy adapter-keyed file (operator reset / tests).
pub fn clear_state(adapter: &str, provider: Option<&str>) -> Result<()> {
    let key = resolve_key(provider, adapter);
    for path in [
        state_file_path(key),
        state_file_path(adapter),
        provider_error_state_file_path(adapter, provider),
    ] {
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
            } else if let Ok(state) = serde_json::from_str::<ProviderErrorHealthState>(&text) {
                if state.degraded {
                    out.push(provider_error_state_as_health(state));
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
    fn generic_exit_one_storm_is_aggregate_infrastructure() {
        let (_env_guard, _home) = isolated_home();
        let adapter = unique_adapter("exit-one-storm");
        let config = quick_config();

        // A normal exit 1 remains a bead failure in isolation. Once the same
        // adapter produces it across unrelated beads, the aggregate detector
        // proves that the shared dispatch path is the more likely cause and
        // trips the adapter without any bead-specific quarantine penalty.
        for n in 0..3 {
            let outcome = record_adapter_failure(
                &adapter,
                None,
                &format!("nd-exit-one-{n}"),
                "exit_code:1",
                &config,
            )
            .unwrap();
            assert!(!outcome.is_infra(), "the first failures stay bead-scoped");
        }

        let tripping =
            record_adapter_failure(&adapter, None, "nd-exit-one-3", "exit_code:1", &config)
                .unwrap();
        assert!(tripping.is_infra(), "the cross-bead exit-1 storm must trip");
        assert!(matches!(
            tripping,
            VerificationRecording::Tripped {
                failures: 4,
                distinct_beads: 4,
                ..
            }
        ));

        let repeated =
            record_adapter_failure(&adapter, None, "nd-exit-one-4", "exit_code:1", &config)
                .unwrap();
        assert!(repeated.is_infra());
        assert!(matches!(
            repeated,
            VerificationRecording::DegradedForThisFingerprint { .. }
        ));

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

    #[test]
    fn claude_outage_replay_counts_errors_and_longest_response_gap() {
        let mut stream = String::from(
            r#"{"type":"assistant","timestamp":1725240000.0,"message":{"role":"assistant"}}
"#,
        );
        for _ in 0..15 {
            stream.push_str(
                r#"{"type":"api_error","timestamp":1725240001.0,"message":"gateway unavailable"}
"#,
            );
        }
        stream.push_str(
            r#"{"type":"assistant","timestamp":1725240613.0,"message":{"role":"assistant"}}
"#,
        );

        let metrics = analyze_claude_stream(&stream);
        assert_eq!(metrics.provider_errors, 15);
        assert_eq!(metrics.provider_error_classes["api_error"], 15);
        assert_eq!(metrics.max_response_gap_ms, Some(613_000));
    }

    #[test]
    fn provider_error_share_has_minimum_sample_and_hysteresis() {
        let (_env_guard, _home) = isolated_home();
        let adapter = unique_adapter("stream-hysteresis");
        let config = ProviderErrorConfig {
            window: std::time::Duration::from_secs(1800),
            degraded_threshold: 0.6,
            restored_threshold: 0.2,
            min_attempts: 5,
        };
        let error = ProviderErrorMetrics {
            provider_errors: 1,
            ..ProviderErrorMetrics::default()
        };
        let healthy = ProviderErrorMetrics::default();

        for _ in 0..4 {
            assert!(matches!(
                record_provider_attempt(&adapter, Some("glm"), &error, config).unwrap(),
                ProviderHealthTransition::None {
                    degraded: false,
                    ..
                }
            ));
        }
        assert!(matches!(
            record_provider_attempt(&adapter, Some("glm"), &error, config).unwrap(),
            ProviderHealthTransition::Degraded { .. }
        ));

        // One healthy attempt must not flap the state back to healthy.
        assert!(matches!(
            record_provider_attempt(&adapter, Some("glm"), &healthy, config).unwrap(),
            ProviderHealthTransition::None { degraded: true, .. }
        ));
        for _ in 0..19 {
            record_provider_attempt(&adapter, Some("glm"), &healthy, config).unwrap();
        }
        assert!(matches!(
            record_provider_attempt(&adapter, Some("glm"), &healthy, config).unwrap(),
            ProviderHealthTransition::Restored { .. }
        ));
        assert!(degraded_state(&adapter, Some("glm")).unwrap().is_none());
        clear_state(&adapter, Some("glm")).unwrap();
    }
}
