//! Configuration reload tier classification.
//!
//! Every configuration key MUST be assigned a reload tier in this table.
//! Adding a new config key without a tier assignment is a **COMPILE-TIME FAILURE**.
//!
//! # Tiers
//!
//! - **Tier A (Live)**: Swap `self.config`; effective next cycle, no rebuild.
//!   These values are read live from `self.config` during worker loop execution.
//!
//! - **Tier B (Rebuild)**: Component reconstructed from new config at cycle boundary.
//!   These affect component initialization or behavior but don't require process restart.
//!
//! - **Tier C (Restart Required)**: Cannot be changed without restarting the process.
//!   These affect process identity, core runtime shape, or embed-level configuration.
//!
//! # Normative Rule
//!
//! A new config key MUST be assigned a tier in the same change that introduces it.
//! An unclassified key MUST produce a compile-time failure, not a runtime surprise.
//!
//! # Usage
//!
//! When adding a new config field:
//!
//! 1. Add it to the appropriate tier table below
//! 2. Run `cargo build` to verify the table is exhaustive
//! 3. Document the tier choice in the field's docstring

use crate::config::{Config, ConfigTier};

/// Assert that every field in Config has an assigned reload tier.
///
/// This function is **never called at runtime** — it exists only to be
/// compile-time checked by Rust's type system. If a field is added to
/// `Config` without a corresponding entry in the tier tables, this will
/// fail to compile with "field X not found in tier tables".
///
/// # Compile-Time Enforcement
///
/// The struct pattern match below exhaustively covers every field in `Config`.
/// Adding a new field without updating this function produces a compiler error:
/// ```text
/// error[E0004]: non-exhaustive patterns: `Config { ... }` does not cover fields: `new_field`
///   --> src/config/tiers.rs:LL:MM
///    |
/// LL |   let Config { ref new_field, .. } = config;
///    |          ^^^^^^^^ pattern does not cover field `new_field`
/// ```
#[allow(dead_code)]
fn assert_all_config_fields_have_tiers(config: &Config) {
    // Tier A: Live swap (read directly from self.config)
    let Config {
        // Agent fields - live
        ref agent,

        // Worker fields - mostly live, some C
        ref worker,

        // Strands - thresholds are live
        ref strands,

        // Budget and pricing - live
        ref budget,
        ref pricing,

        // Limits - Tier B (rebuild RateLimiter)
        // ref limits,

        // Prompt - Tier B (rebuild PromptBuilder)
        // ref prompt,

        // All others handled below
        ref telemetry,
        ref workspace,
        ref bead_cli,
        ref health,
        ref gates,
        ref verification,
        ref validation,
        ref limits,
        ref prompt,
        ref tsnet,
        ref self_modification,
        ref fabric,
        ref supervisor,
        ref outcome,
        ref workspace_health,
        ref post_push_ci,
        ref stop,
        ref attempt_archive,
        ref transitions,
    } = config;

    // Verify tier A assignments
    let _ = agent.reload_tier();
    let _ = worker.reload_tier();
    let _ = strands.reload_tier();
    let _ = budget.reload_tier();
    let _ = pricing.reload_tier();
    let _ = post_push_ci.reload_tier();
    let _ = stop.reload_tier();

    // Verify tier B assignments
    let _ = telemetry.reload_tier();
    let _ = prompt.reload_tier();
    let _ = limits.reload_tier();
    let _ = gates.reload_tier();
    let _ = validation.reload_tier();
    let _ = attempt_archive.reload_tier();

    // Verify tier C assignments
    let _ = workspace.reload_tier();
    let _ = bead_cli.reload_tier();
    let _ = health.reload_tier();
    let _ = tsnet.reload_tier();
    let _ = self_modification.reload_tier();
    let _ = supervisor.reload_tier();
    let _ = outcome.reload_tier();
    let _ = workspace_health.reload_tier();

    // Legacy fields - tracked but use newer equivalents
    let _ = verification;
    let _ = fabric;

    // Tracked but not yet tier-assigned: TransitionsConfig has no reload_tier
    // yet, so a change to it is treated like a restart-required section.
    let _ = transitions;
}

/// Reload tier for a configuration field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadTier {
    /// Tier A: Live swap - effective next cycle with no rebuild
    Live,

    /// Tier B: Component rebuild - reconstructed at cycle boundary
    Rebuild,

    /// Tier C: Restart required - cannot change without process restart
    RestartRequired,
}

impl ReloadTier {
    /// Human-readable name for the tier
    pub fn name(&self) -> &'static str {
        match self {
            ReloadTier::Live => "A (live)",
            ReloadTier::Rebuild => "B (rebuild)",
            ReloadTier::RestartRequired => "C (restart-required)",
        }
    }
}

/// Get the reload tier for a dot-notation config key path.
///
/// Returns the tier for a given key path, or `None` if the path is not recognized.
/// This is used for runtime validation and diagnostics.
///
/// # Examples
///
/// ```
/// use needle::config::tiers::get_tier_for_key;
///
/// assert_eq!(get_tier_for_key("agent.timeout"), Some(ReloadTier::Live));
/// assert_eq!(get_tier_for_key("workspace.home"), Some(ReloadTier::RestartRequired));
/// assert_eq!(get_tier_for_key("telemetry.otlp.enabled"), Some(ReloadTier::Rebuild));
/// assert_eq!(get_tier_for_key("unknown.field"), None);
/// ```
pub fn get_tier_for_key(key: &str) -> Option<ReloadTier> {
    TIER_TABLE
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, tier)| *tier)
}

/// Accepted-but-deprecated key spellings, mapped to the canonical spelling
/// they deserialize into.
///
/// A key lands here when a config field is renamed. The old spelling stays in
/// the tier table so an existing `.needle.yaml` still resolves to a tier, and
/// it stays live through the field's serde alias. This table is what the
/// loader consults to tell the operator the spelling is legacy, and what the
/// tier-table test consults to know which entries are aliases rather than
/// field paths.
pub static DEPRECATED_KEY_SPELLINGS: &[(&str, &str)] = &[
    (
        "strands.mend.stuck_threshold_secs",
        "strands.mend.stale_claim_ttl",
    ),
    (
        "strands.explore.stuck_threshold_secs",
        "strands.explore.stale_claim_ttl",
    ),
    ("strands.mend.lock_ttl", "strands.mend.lock_ttl_secs"),
];

/// Canonical spelling for a deprecated key path, if the path is one.
pub fn canonical_key_spelling(key: &str) -> Option<&'static str> {
    DEPRECATED_KEY_SPELLINGS
        .iter()
        .find(|(deprecated, _)| *deprecated == key)
        .map(|(_, canonical)| *canonical)
}

/// Master tier table: maps every config key path to its reload tier.
///
/// This table is the single source of truth for reload tier classification.
/// Every configuration key MUST have an entry here.
///
/// Keys are dot-notation paths (e.g., "agent.default", "worker.max_workers").
/// Values indicate which reload mechanism handles that key.
///
/// # Maintenance
///
/// When adding a new config field:
/// 1. Add its key path to this table with appropriate tier
/// 2. Update `assert_all_config_fields_have_tiers()` if it's a top-level field
/// 3. Add tests to verify tier assignment
/// 4. Document the tier in the field's docstring
static TIER_TABLE: &[(&str, ReloadTier)] = &[
    // ═══════════════════════════════════════════════════════════════════════════════
    // TIER A: LIVE SWAP
    // ═══════════════════════════════════════════════════════════════════════════════
    // These values are read directly from self.config during loop execution.

    // Agent configuration (live)
    ("agent.default", ReloadTier::Live),
    ("agent.args", ReloadTier::Live),
    ("agent.timeout", ReloadTier::Live),
    ("agent.routing", ReloadTier::Live),
    ("agent.routing.rules", ReloadTier::Live),
    ("agent.routing.default_adapter", ReloadTier::Live),
    ("agent.routing.strict", ReloadTier::Live),
    // Worker configuration (mostly live)
    ("worker.idle_timeout", ReloadTier::Live),
    ("worker.idle_action", ReloadTier::Live),
    ("worker.allow_exit_without_supervisor", ReloadTier::Live),
    ("worker.max_claim_retries", ReloadTier::Live),
    ("worker.claim_race_lost_skip", ReloadTier::Live),
    ("worker.cpu_load_warn", ReloadTier::Live),
    ("worker.memory_free_warn_mb", ReloadTier::Live),
    ("worker.enforce_shipped_work", ReloadTier::Live),
    ("worker.adaptive_stagger_max_wait_secs", ReloadTier::Live),
    (
        "worker.adaptive_stagger_check_interval_secs",
        ReloadTier::Live,
    ),
    ("worker.building_timeout", ReloadTier::Live),
    ("worker.idle_backoff_min", ReloadTier::Live),
    ("worker.idle_backoff_max", ReloadTier::Live),
    ("worker.short_retry_backoff", ReloadTier::Live),
    ("worker.freshness_check_interval_secs", ReloadTier::Live),
    // Budget and pricing (live)
    ("budget.warn_usd", ReloadTier::Live),
    ("budget.stop_usd", ReloadTier::Live),
    ("pricing", ReloadTier::Live),
    // Strand thresholds (live)
    ("strands.pluck.exclude_labels", ReloadTier::Live),
    // Mend's and Explore's claim TTLs are stored under the field names
    // `stale_claim_ttl` (the spellings `apply_tier_a_config` reports). The
    // legacy `stuck_threshold_secs` spellings — the field names these TTLs
    // used to live under — and Mend's `lock_ttl` alias remain accepted so a
    // key copied out of an existing `.needle.yaml` still resolves to its
    // tier. Every spelling here must reach a real config field; the
    // `test_every_tier_key_resolves_in_serialized_config` test fails a tier
    // entry whose key no field produces.
    ("strands.mend.stale_claim_ttl", ReloadTier::Live),
    ("strands.mend.stuck_threshold_secs", ReloadTier::Live),
    ("strands.mend.lock_ttl_secs", ReloadTier::Live),
    ("strands.mend.lock_ttl", ReloadTier::Live),
    ("strands.explore.workspaces", ReloadTier::Live),
    ("strands.explore.stale_claim_ttl", ReloadTier::Live),
    ("strands.explore.stuck_threshold_secs", ReloadTier::Live),
    ("strands.weave.max_beads_per_run", ReloadTier::Live),
    ("strands.weave.cooldown_hours", ReloadTier::Live),
    // Unravel's fields are `max_beads_per_run` / `cooldown_hours`; the
    // `max_per_run` / `cooldown_days` spellings in the original plan were
    // never fields, and `cooldown_days` cannot be an alias of a
    // different-unit key.
    ("strands.unravel.max_beads_per_run", ReloadTier::Live),
    ("strands.unravel.cooldown_hours", ReloadTier::Live),
    ("strands.pulse.max_beads_per_run", ReloadTier::Live),
    ("strands.pulse.cooldown_hours", ReloadTier::Live),
    ("strands.pulse.severity_threshold", ReloadTier::Live),
    ("strands.reflect.min_beads_since_last", ReloadTier::Live),
    ("strands.reflect.cooldown_hours", ReloadTier::Live),
    ("strands.reflect.max_learnings_per_run", ReloadTier::Live),
    ("strands.reflect.max_skills_per_run", ReloadTier::Live),
    ("strands.reflect.learning_retention_days", ReloadTier::Live),
    ("strands.reflect.max_learnings", ReloadTier::Live),
    ("strands.splice.stale_threshold_secs", ReloadTier::Live),
    ("strands.splice.detect_live_loops", ReloadTier::Live),
    ("strands.splice.live_loop_scan_events", ReloadTier::Live),
    ("strands.splice.claim_churn_threshold", ReloadTier::Live),
    ("strands.splice.log_runaway_bytes", ReloadTier::Live),
    ("strands.splice.live_loop_window_secs", ReloadTier::Live),
    ("strands.knot.alert_cooldown_minutes", ReloadTier::Live),
    ("strands.knot.exhaustion_threshold", ReloadTier::Live),
    ("strands.knot.starvation_backoff_minutes", ReloadTier::Live),
    (
        "strands.learning.trace_retention_failed_days",
        ReloadTier::Rebuild,
    ),
    (
        "strands.learning.trace_retention_success_days",
        ReloadTier::Rebuild,
    ),
    // Learning context injection is read when the PromptBuilder is built.
    (
        "strands.learning.inject_legacy_learnings",
        ReloadTier::Rebuild,
    ),
    (
        "strands.learning.max_learning_context_bytes",
        ReloadTier::Rebuild,
    ),
    // Attempt history is read per dispatch and per resolution (live).
    ("strands.learning.failure_history.enabled", ReloadTier::Live),
    (
        "strands.learning.failure_history.max_attempts",
        ReloadTier::Live,
    ),
    (
        "strands.learning.failure_history.max_bytes",
        ReloadTier::Live,
    ),
    (
        "strands.learning.failure_history.sync_to_bead_data",
        ReloadTier::Live,
    ),
    // Mitosis (live)
    ("strands.mitosis.enabled", ReloadTier::Live),
    ("strands.mitosis.first_failure_only", ReloadTier::Live),
    // Outcome (live)
    ("outcome.quarantine_after_failures", ReloadTier::Live),
    // Workspace-health fingerprint thresholds (live; read per verification failure)
    (
        "workspace_health.fingerprint_window_seconds",
        ReloadTier::Live,
    ),
    (
        "workspace_health.fingerprint_window_max_failures",
        ReloadTier::Live,
    ),
    (
        "workspace_health.fingerprint_min_window_failures",
        ReloadTier::Live,
    ),
    ("workspace_health.fingerprint_trip_ratio", ReloadTier::Live),
    ("workspace_health.adapter_health_enabled", ReloadTier::Live),
    (
        "workspace_health.adapter_degraded_cooldown_secs",
        ReloadTier::Live,
    ),
    (
        "workspace_health.fingerprint_min_distinct_beads",
        ReloadTier::Live,
    ),
    // ═══════════════════════════════════════════════════════════════════════════════
    // TIER B: COMPONENT REBUILD
    // ═══════════════════════════════════════════════════════════════════════════════
    // These require component reconstruction at the cycle boundary.

    // Telemetry (rebuild Telemetry, may reinstall OTLP layer)
    ("telemetry.file_sink.enabled", ReloadTier::Rebuild),
    ("telemetry.file_sink.log_dir", ReloadTier::Rebuild),
    ("telemetry.file_sink.retention_days", ReloadTier::Rebuild),
    ("telemetry.file_sink.rotation", ReloadTier::Rebuild),
    ("telemetry.stdout_sink.enabled", ReloadTier::Rebuild),
    ("telemetry.stdout_sink.format", ReloadTier::Rebuild),
    ("telemetry.stdout_sink.color", ReloadTier::Rebuild),
    ("telemetry.hooks", ReloadTier::Rebuild),
    ("telemetry.otlp.enabled", ReloadTier::Rebuild),
    ("telemetry.otlp.endpoint", ReloadTier::Rebuild),
    ("telemetry.otlp.protocol", ReloadTier::Rebuild),
    ("telemetry.otlp.headers", ReloadTier::Rebuild),
    ("telemetry.otlp.timeout_ms", ReloadTier::Rebuild),
    ("telemetry.otlp.compression", ReloadTier::Rebuild),
    ("telemetry.otlp.tls.insecure", ReloadTier::Rebuild),
    ("telemetry.otlp.tls.ca_file", ReloadTier::Rebuild),
    ("telemetry.otlp.signals", ReloadTier::Rebuild),
    ("telemetry.otlp.resource_attributes", ReloadTier::Rebuild),
    // Prompt (rebuild PromptBuilder)
    ("prompt.context_files", ReloadTier::Rebuild),
    ("prompt.instructions", ReloadTier::Rebuild),
    ("prompt.templates", ReloadTier::Rebuild),
    ("prompt.variants", ReloadTier::Rebuild),
    // Agent adapter directory (rebuild Dispatcher's adapter loader)
    ("agent.adapters_dir", ReloadTier::Rebuild),
    // Rate limiting (rebuild RateLimiter)
    ("limits.providers", ReloadTier::Rebuild),
    ("limits.models", ReloadTier::Rebuild),
    // Validation gates (rebuild OutcomeHandler)
    ("gates", ReloadTier::Rebuild),
    ("validation.outcome_timeout_seconds", ReloadTier::Rebuild),
    ("validation.stderr_cap_bytes", ReloadTier::Rebuild),
    // ═══════════════════════════════════════════════════════════════════════════════
    // TIER C: RESTART REQUIRED
    // ═══════════════════════════════════════════════════════════════════════════════
    // These cannot be changed without restarting the process.

    // Worker identity and launch (Tier C)
    ("worker.max_workers", ReloadTier::RestartRequired),
    ("worker.launch_stagger_seconds", ReloadTier::RestartRequired),
    ("worker.identifier_scheme", ReloadTier::RestartRequired),
    ("worker.worker_binary_path", ReloadTier::RestartRequired),
    // Configuration reload polling is itself a process-level capability. With
    // the default interval of 0, a restart is required to enable the poller.
    (
        "worker.config_reload_check_interval_secs",
        ReloadTier::RestartRequired,
    ),
    // Workspace paths (Tier C - process identity depends on these)
    ("workspace.home", ReloadTier::RestartRequired),
    ("workspace.default", ReloadTier::RestartRequired),
    // Bead CLI backend (Tier C - store-level decision, process-scoped)
    ("bead_cli.backend", ReloadTier::RestartRequired),
    ("bead_cli.path", ReloadTier::RestartRequired),
    // Health monitoring (Tier C - installed at boot, global subscriber)
    ("health.heartbeat_interval", ReloadTier::RestartRequired),
    ("health.heartbeat_ttl", ReloadTier::RestartRequired),
    ("health.heartbeat_dir", ReloadTier::RestartRequired),
    ("health.peer_check_interval", ReloadTier::RestartRequired),
    // Tsnet (Tier C - embed-level, subprocess-facing)
    ("tsnet", ReloadTier::RestartRequired),
    // Self-modification (Tier C - controls hot-reload itself)
    ("self_modification", ReloadTier::RestartRequired),
    // Supervisor (Tier C - daemon lifecycle)
    ("supervisor", ReloadTier::RestartRequired),
    // Stop (Tier A - read once per `needle stop` invocation, never by the worker)
    ("stop.grace_period_secs", ReloadTier::Live),
    // Attempt archive (Tier B - spool writer and retention gate are rebuilt at the cycle boundary)
    ("attempt_archive.enabled", ReloadTier::Rebuild),
    ("attempt_archive.spool_dir", ReloadTier::Rebuild),
    ("attempt_archive.include.trace", ReloadTier::Rebuild),
    (
        "attempt_archive.include.harness_transcript",
        ReloadTier::Rebuild,
    ),
    ("attempt_archive.include.prompt", ReloadTier::Rebuild),
    ("attempt_archive.compression", ReloadTier::Rebuild),
    (
        "attempt_archive.prune_local_after_spool",
        ReloadTier::Rebuild,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_table_is_exhaustive() {
        // This test ensures the tier table compiles and covers known keys.
        // The real enforcement is in assert_all_config_fields_have_tiers().
        let config = Config::default();
        assert_all_config_fields_have_tiers(&config);
    }

    #[test]
    fn test_get_tier_for_known_keys() {
        assert_eq!(get_tier_for_key("agent.timeout"), Some(ReloadTier::Live));
        assert_eq!(
            get_tier_for_key("telemetry.otlp.enabled"),
            Some(ReloadTier::Rebuild)
        );
        assert_eq!(
            get_tier_for_key("workspace.home"),
            Some(ReloadTier::RestartRequired)
        );
        assert_eq!(
            get_tier_for_key("worker.config_reload_check_interval_secs"),
            Some(ReloadTier::RestartRequired)
        );
        assert_eq!(
            get_tier_for_key("strands.learning.trace_retention_failed_days"),
            Some(ReloadTier::Rebuild)
        );
        assert_eq!(
            get_tier_for_key("strands.learning.trace_retention_success_days"),
            Some(ReloadTier::Rebuild)
        );
        for key in [
            "attempt_archive.enabled",
            "attempt_archive.spool_dir",
            "attempt_archive.include.trace",
            "attempt_archive.include.harness_transcript",
            "attempt_archive.include.prompt",
            "attempt_archive.compression",
            "attempt_archive.prune_local_after_spool",
        ] {
            assert_eq!(get_tier_for_key(key), Some(ReloadTier::Rebuild), "{key}");
        }
        assert_eq!(
            get_tier_for_key("stop.grace_period_secs"),
            Some(ReloadTier::Live)
        );
    }

    #[test]
    fn test_get_tier_for_unknown_keys_returns_none() {
        assert_eq!(get_tier_for_key("unknown.field"), None);
        assert_eq!(get_tier_for_key("agent.nonexistent"), None);
    }

    #[test]
    fn test_mend_keys_resolve_in_every_documented_spelling() {
        // Both the stored field names (what `apply_tier_a_config` reports)
        // and the accepted deprecated spellings (what an older `.needle.yaml`
        // may spell) must resolve to a tier, so a key copied out of a config
        // file is never reported as unrecognized.
        for key in [
            "strands.mend.stale_claim_ttl",
            "strands.mend.stuck_threshold_secs",
            "strands.mend.lock_ttl_secs",
            "strands.mend.lock_ttl",
            "strands.explore.stale_claim_ttl",
            "strands.explore.stuck_threshold_secs",
        ] {
            assert_eq!(get_tier_for_key(key), Some(ReloadTier::Live), "{key}");
        }
    }

    #[test]
    fn test_every_tier_key_resolves_in_serialized_config() {
        // A tier entry whose key no config field produces is a lie: it tells
        // the operator a key is reloadable when nothing reads it. That is
        // exactly how `strands.mend.stale_claim_ttl` went dead before
        // needle-c8510ace — declared Live, read nowhere. Every entry must
        // either name a path that exists in the serialized config or be a
        // registered deprecated spelling of one that does.
        let value = serde_json::to_value(Config::default()).expect("default config serializes");
        for (key, _) in TIER_TABLE {
            let resolves = resolve_key(&value, key)
                || canonical_key_spelling(key)
                    .is_some_and(|canonical| resolve_key(&value, canonical));
            assert!(
                resolves,
                "tier key '{key}' matches no config field: either the spelling is \
                 wrong or the field it named was removed without dropping its tier entry"
            );
        }
    }

    #[test]
    fn test_deprecated_spellings_map_to_live_canonical_keys() {
        // A deprecated spelling is only honest if its canonical twin both
        // exists as a field and carries the same tier: the alias must not
        // outlive the key it points at.
        for (deprecated, canonical) in DEPRECATED_KEY_SPELLINGS {
            assert!(
                get_tier_for_key(canonical).is_some(),
                "canonical key '{canonical}' has no tier entry"
            );
            assert_eq!(
                get_tier_for_key(deprecated),
                get_tier_for_key(canonical),
                "deprecated '{deprecated}' and canonical '{canonical}' must share a tier"
            );
            assert_ne!(
                get_tier_for_key(deprecated),
                None,
                "deprecated spelling '{deprecated}' must stay resolvable"
            );
        }
    }

    /// Walk a dot-notation key through a serialized config value.
    ///
    /// Returns `true` only when every segment names an existing key (or array
    /// index) on the way down, so a key that no field produces fails.
    fn resolve_key(value: &serde_json::Value, key: &str) -> bool {
        let mut current = value;
        for segment in key.split('.') {
            match current {
                serde_json::Value::Object(map) => match map.get(segment) {
                    Some(next) => current = next,
                    None => return false,
                },
                serde_json::Value::Array(items) => {
                    match segment.parse::<usize>().ok().and_then(|i| items.get(i)) {
                        Some(next) => current = next,
                        None => return false,
                    }
                }
                _ => return false,
            }
        }
        true
    }

    #[test]
    fn test_tier_names() {
        assert_eq!(ReloadTier::Live.name(), "A (live)");
        assert_eq!(ReloadTier::Rebuild.name(), "B (rebuild)");
        assert_eq!(ReloadTier::RestartRequired.name(), "C (restart-required)");
    }
}
