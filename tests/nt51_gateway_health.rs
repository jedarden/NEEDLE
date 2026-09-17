//! Focused behavioral contracts for N-T51 (plan revision 34 sections
//! 4.9–4.10; ADR-029, ADR-030): provider health keyed by the adapter's
//! gateway.
//!
//! The bead's acceptance command is this target. The ledger evidence behind
//! it (2026-09-12..14): `claude-code-glm-5.3`, `claude-code-glm-5.3-flash`,
//! `opencode-glm-5.3-flash` and `omp-glm-5.3-flash` all talk through
//! `zai-proxy`, but health was keyed per adapter, so a storm on one left its
//! siblings routable. Three behaviors live here:
//!
//! 1. A storm on one adapter behind a gateway degrades every adapter behind
//!    that gateway — not an adapter on another one — and one verified
//!    success on any of them restores the whole group.
//! 2. Adapters that declare no provider keep per-adapter health exactly as
//!    before, and the keying ships off by default
//!    (`WorkspaceHealthConfig::provider_keyed_health`).
//! 3. Adapter-keyed state files written before the keying migrate into the
//!    provider-keyed file without losing an active degradation.
//!
//! Isolation: these tests run in-process and swap `HOME` (and clear
//! `NEEDLE_STATE_DIR`) under a process-wide lock, so provider-health state
//! resolves beneath a per-test temp root and never touches the live
//! `~/.needle`. Nothing here spawns NEEDLE, reaches a bead store, or emits
//! telemetry.

use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard, OnceLock};

use tempfile::TempDir;

use needle::gate_health::VerificationRecording;
use needle::provider_health::{
    clear_state, degraded_adapters, degraded_state, expand_degraded_adapters,
    record_adapter_failure, record_adapter_success, state_file_path,
};
use needle::verification_fingerprint::DetectorConfig;

/// The gateway from the ledger evidence, and the adapters behind it.
const GATEWAY: &str = "zai-proxy";
const STORM: &str = "claude-code-glm-5.3";
const SIBLING: &str = "opencode-glm-5.3-flash";
const COUSIN: &str = "omp-glm-5.3-flash";
/// An adapter on a different gateway entirely.
const OUTSIDER: &str = "codex";

/// The single process-wide lock guarding the environment this harness swaps.
///
/// Provider health resolves its state root from the environment at call
/// time, so one test's leftover `HOME` would decide another test's result.
static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn env_lock() -> MutexGuard<'static, ()> {
    let lock = ENV_LOCK.get_or_init(|| Mutex::new(()));
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Restores the swapped variables when dropped, then releases the lock —
/// restore-before-unlock, the order the in-crate `test_env` guard learned
/// the hard way.
struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl EnvGuard {
    /// Take the lock and point provider health at a fresh per-test root.
    fn take() -> (Self, TempDir) {
        let lock = env_lock();
        let saved = ["HOME", "NEEDLE_STATE_DIR"]
            .iter()
            .map(|&key| (key, std::env::var_os(key)))
            .collect();
        let home = TempDir::new().expect("create the isolated home");
        std::env::set_var("HOME", home.path());
        // Belt and braces: an inherited override would beat HOME.
        std::env::remove_var("NEEDLE_STATE_DIR");
        (Self { saved, _lock: lock }, home)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// Detector thresholds small enough to trip inside one test: the same
/// failure across three distinct beads within a tight window.
fn quick_config() -> DetectorConfig {
    DetectorConfig {
        window: std::time::Duration::from_secs(3600),
        window_max_failures: 20,
        min_window_failures: 4,
        trip_ratio: 0.75,
        min_distinct_beads: 3,
    }
}

/// Drive one adapter's failures until the detector trips, and return the
/// tripped fingerprint.
fn storm(adapter: &str, provider: Option<&str>, reason: &str, config: &DetectorConfig) -> String {
    let mut fingerprint = None;
    for n in 0..4 {
        let recording = record_adapter_failure(
            adapter,
            provider,
            &format!("needle-storm-{n}"),
            reason,
            config,
        )
        .expect("record the failure");
        match &recording {
            VerificationRecording::Tripped { fingerprint, .. } => {
                return fingerprint.clone();
            }
            _ => fingerprint = Some(recording.fingerprint().to_string()),
        }
    }
    panic!(
        "the storm never tripped for {adapter}; last recording: {:?}",
        fingerprint
    );
}

// ─── One gateway, one fate ───────────────────────────────────────────────────

#[test]
fn a_storm_on_one_gateway_adapter_degrades_its_siblings_and_not_codex() {
    let (_env, _home) = EnvGuard::take();
    let config = quick_config();
    let fingerprint = storm(STORM, Some(GATEWAY), "exit_code:124", &config);
    assert!(!fingerprint.is_empty());

    // Every adapter behind the gateway is degraded through the provider's
    // one state file — including the two that never failed.
    for adapter in [STORM, SIBLING, COUSIN] {
        let state = degraded_state(adapter, Some(GATEWAY))
            .unwrap()
            .unwrap_or_else(|| panic!("{adapter} behind {GATEWAY} should be degraded"));
        assert_eq!(state.health_key(), GATEWAY);
    }

    // An adapter on another gateway — with or without its own provider — is
    // untouched: its health key names a file that was never written.
    assert!(
        degraded_state(OUTSIDER, None).unwrap().is_none(),
        "{OUTSIDER} declares no provider and keeps its own health"
    );
    assert!(
        degraded_state(OUTSIDER, Some("openai")).unwrap().is_none(),
        "{OUTSIDER} sits on a different gateway"
    );

    // The same failure shape on a sibling is the provider's, not the bead's:
    // behind one gateway the fingerprint hashes the key, so the sibling's
    // storm failures resolve as infrastructure the moment the group trips.
    let sibling_failure = record_adapter_failure(
        SIBLING,
        Some(GATEWAY),
        "needle-sibling-same-shape",
        "exit_code:124",
        &config,
    )
    .unwrap();
    assert!(
        matches!(
            sibling_failure,
            VerificationRecording::DegradedForThisFingerprint { .. }
        ),
        "{sibling_failure:?}"
    );
    assert!(sibling_failure.is_infra());

    // A different failure on the sibling is still judged as the bead's own.
    let other = record_adapter_failure(
        SIBLING,
        Some(GATEWAY),
        "needle-sibling-other",
        "exit_code:1",
        &config,
    )
    .unwrap();
    assert!(
        matches!(other, VerificationRecording::DegradedForOther { .. }),
        "{other:?}"
    );
    assert!(!other.is_infra());

    // Routing evidence sees the whole group degraded at once: one state
    // file for the gateway, expanded to every configured adapter behind it.
    let states = degraded_adapters();
    assert_eq!(states.len(), 1, "one gateway, one state file: {states:?}");
    assert_eq!(states[0].health_key(), GATEWAY);
    let expanded = expand_degraded_adapters(
        &states,
        [
            (STORM.to_string(), Some(GATEWAY.to_string())),
            (SIBLING.to_string(), Some(GATEWAY.to_string())),
            (COUSIN.to_string(), Some(GATEWAY.to_string())),
            (OUTSIDER.to_string(), Some("openai".to_string())),
            ("claude-print".to_string(), None),
        ],
    );
    for degraded in [STORM, SIBLING, COUSIN] {
        assert!(
            expanded.contains(&degraded.to_string()),
            "{degraded} should be expanded from the degraded gateway: {expanded:?}"
        );
    }
    for healthy in [OUTSIDER, "claude-print"] {
        assert!(
            !expanded.contains(&healthy.to_string()),
            "{healthy} is not behind {GATEWAY}: {expanded:?}"
        );
    }

    // One verified success on any sibling restores the whole group.
    let prior = record_adapter_success(SIBLING, Some(GATEWAY))
        .unwrap()
        .expect("the group was degraded");
    assert_eq!(prior.health_key(), GATEWAY);
    assert!(prior.affected_adapters().contains(&STORM.to_string()));
    for adapter in [STORM, SIBLING, COUSIN] {
        assert!(
            degraded_state(adapter, Some(GATEWAY)).unwrap().is_none(),
            "{adapter} should be restored with the group"
        );
    }
}

#[test]
fn adapters_without_a_provider_keep_per_adapter_health() {
    let (_env, _home) = EnvGuard::take();
    let config = quick_config();
    storm("claude-print", None, "exit_code:124", &config);

    // The storm degrades its adapter and nothing else.
    assert!(degraded_state("claude-print", None).unwrap().is_some());
    assert!(degraded_state("codex", None).unwrap().is_none());

    // Expansion without providers is per adapter, exactly as before N-T51.
    let states = degraded_adapters();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].health_key(), "claude-print");
    let expanded = expand_degraded_adapters(
        &states,
        [
            ("claude-print".to_string(), None),
            ("codex".to_string(), None),
        ],
    );
    assert_eq!(expanded, vec!["claude-print".to_string()]);

    // Success restores exactly that adapter.
    assert!(record_adapter_success("claude-print", None)
        .unwrap()
        .is_some());
    assert!(degraded_state("claude-print", None).unwrap().is_none());
    clear_state("claude-print", None).unwrap();
}

// ─── The keying ships off by default ─────────────────────────────────────────

#[test]
fn gateway_keying_ships_off_by_default() {
    // The Default and a config section that predates the flag both leave it
    // off, so the shipped behavior stays per-adapter (N-T23) until the
    // plan's activation order turns gateway keying on.
    let default = needle::config::WorkspaceHealthConfig::default();
    assert!(!default.provider_keyed_health);
    let pre_flag: needle::config::WorkspaceHealthConfig =
        serde_json::from_str("{}").expect("every field carries a serde default");
    assert!(!pre_flag.provider_keyed_health);
}

// ─── Migration of adapter-keyed state files ──────────────────────────────────

#[test]
fn adapter_keyed_state_migrates_to_the_gateway_key_without_losing_degradation() {
    let (_env, _home) = EnvGuard::take();
    let config = quick_config();

    // A state file written before N-T51: adapter-keyed (no `provider` or
    // `adapters` keys), actively degraded from an earlier storm, with its
    // last degraded failure recent enough that the claim hold still bites.
    let now = chrono::Utc::now();
    let a_minute_ago = (now - chrono::Duration::minutes(1)).to_rfc3339();
    let legacy = serde_json::json!({
        "adapter": STORM,
        "window": [
            { "at": a_minute_ago, "bead": "needle-legacy-1", "fingerprint": "legacy-fp" },
            { "at": a_minute_ago, "bead": "needle-legacy-2", "fingerprint": "legacy-fp" },
        ],
        "degraded": true,
        "degraded_fingerprint": "legacy-fp",
        "degraded_summary": "exit code 124",
        "degraded_at": a_minute_ago,
        "last_degraded_failure_at": a_minute_ago,
    });
    let legacy_path = state_file_path(STORM);
    std::fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
    std::fs::write(&legacy_path, legacy.to_string()).unwrap();

    // The read path holds the adapter before any write adopts the file: the
    // degradation that predates the upgrade still holds the adapter that
    // tripped it, and the hold clock still measures from its last failure.
    let held = degraded_state(STORM, Some(GATEWAY))
        .unwrap()
        .expect("the legacy degradation still holds");
    assert_eq!(held.degraded_fingerprint.as_deref(), Some("legacy-fp"));
    assert!(
        held.secs_since_last_degraded_failure()
            .is_some_and(|s| s < 3600),
        "the hold measures from the legacy failure: {:?}",
        held.secs_since_last_degraded_failure()
    );
    let legacy_states = degraded_adapters();
    let expanded_before_migration = expand_degraded_adapters(
        &legacy_states,
        [
            (STORM.to_string(), Some(GATEWAY.to_string())),
            (SIBLING.to_string(), Some(GATEWAY.to_string())),
            (COUSIN.to_string(), Some(GATEWAY.to_string())),
        ],
    );
    assert!(
        expanded_before_migration.contains(&SIBLING.to_string()),
        "legacy degradation expands to the gateway before its first write: {expanded_before_migration:?}"
    );

    // The first provider-keyed record adopts the file: the adapter-keyed
    // file is gone, the gateway-keyed one carries the degradation, the
    // member, and the failure history.
    let recording = record_adapter_failure(
        STORM,
        Some(GATEWAY),
        "needle-after-upgrade",
        "exit_code:124",
        &config,
    )
    .unwrap();
    // The legacy fingerprint is not this failure's, so the failure is
    // judged as the bead's own — and the degradation survives it.
    assert!(
        matches!(recording, VerificationRecording::DegradedForOther { .. }),
        "{recording:?}"
    );
    assert!(!recording.is_infra());
    assert!(
        !legacy_path.exists(),
        "the adapter-keyed file must not linger after the migration"
    );
    let migrated = degraded_state(STORM, Some(GATEWAY))
        .unwrap()
        .expect("the degradation survived the migration");
    assert_eq!(migrated.health_key(), GATEWAY);
    assert_eq!(migrated.degraded_fingerprint.as_deref(), Some("legacy-fp"));
    assert!(migrated.affected_adapters().contains(&STORM.to_string()));
    // The legacy window rides along — recent entries, so the detector keeps
    // its history — alongside the failure that triggered the migration.
    assert!(
        migrated.window.len() >= 3,
        "legacy window entries survive the migration: {:?}",
        migrated.window
    );

    // A verified success on the migrated group — from any member — lifts it.
    assert!(record_adapter_success(SIBLING, Some(GATEWAY))
        .unwrap()
        .is_some());
    assert!(degraded_state(STORM, Some(GATEWAY)).unwrap().is_none());
}
