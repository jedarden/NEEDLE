//! Adapter activity input for worker heartbeats.
//!
//! Incident context (needle-ded172f0): a dispatched agent process kept making
//! model and tool calls while the heartbeat exposed no progress, and Mend /
//! Explore treated the stale bead `updated_at` timestamp as inactivity and
//! let the same bead be dispatched again. Bead-rs claims are the ownership
//! boundary, not a high-frequency progress bus — so meaningful model/tool
//! progress needs its own, attempt-scoped channel that lands in the
//! heartbeat, not in the bead store.
//!
//! The rules that make this safe:
//!
//! - **NEEDLE binds, the adapter reports.** A reporter is created for one
//!   dispatch attempt and one bead. The adapter supplies only an
//!   [`ActivityKind`]; NEEDLE attaches the worker (implicitly — each worker
//!   writes only its own heartbeat file), the attempt, the bead, the
//!   sequence number, and the observed time.
//! - **Stale attempts lose.** An event whose attempt or bead does not match
//!   the dispatch currently bound in the heartbeat state is dropped. A late
//!   event from a prior attempt can never refresh a successor attempt.
//! - **Deduplication by sequence.** The reporter assigns a monotonically
//!   increasing sequence per report; an event whose sequence is not strictly
//!   newer than the retained one (duplicate or reordered arrival) is
//!   dropped. A forged timestamp is harmless because the observed time is
//!   re-stamped at ingest — the submitted value is never trusted.
//! - **No bead-store writes.** Reporting touches only the in-memory
//!   heartbeat slot. It never calls [`crate::bead_store::BeadStore`], never
//!   updates `bead.updated_at`, never renews or reclaims a claim, never
//!   advances the bead audit sequence, and never publishes a checkpoint.
//!   The module dependency graph enforces this structurally: `health`
//!   depends only on `config`, `telemetry`, and `types`.
//! - **Bounded and payload-free.** The record has a fixed field set — no
//!   prompts, tool arguments, model output, secrets, or arbitrary payloads
//!   are accepted or persisted. High-rate reporting coalesces naturally:
//!   each report overwrites one in-memory slot and the emitter persists at
//!   most one small file per heartbeat interval regardless of volume.
//! - **Backward compatible.** The heartbeat field is optional; heartbeats
//!   written before it existed parse with no activity, and adapters that
//!   never report keep the previous behavior byte-for-byte.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::BeadId;

use super::SharedHeartbeatState;

/// Maximum retained size for an attempt identity in a heartbeat record.
pub const MAX_ACTIVITY_ATTEMPT_ID_LEN: usize = 128;
/// Maximum retained size for a bead identity in a heartbeat record.
pub const MAX_ACTIVITY_BEAD_ID_LEN: usize = 256;

fn identifiers_are_bounded(attempt_id: &str, bead_id: &BeadId) -> bool {
    attempt_id.len() <= MAX_ACTIVITY_ATTEMPT_ID_LEN
        && bead_id.as_ref().len() <= MAX_ACTIVITY_BEAD_ID_LEN
}

// ──────────────────────────────────────────────────────────────────────────────
// ActivityKind — bounded, payload-free vocabulary
// ──────────────────────────────────────────────────────────────────────────────

/// The bounded set of adapter activity kinds a reporter may report.
///
/// This is a closed enum, not a string: an adapter integration cannot inject
/// an arbitrary label, and there is no payload field to carry one. The
/// `Unknown` variant exists only for forward compatibility — a heartbeat
/// written by a newer NEEDLE with a kind this build does not know parses as
/// `Unknown` instead of failing the whole heartbeat file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    /// A model/API call is in progress (the agent is thinking or streaming).
    ModelCall,
    /// A tool invocation is in progress.
    ToolCall,
    /// A kind written by a newer NEEDLE build. Never emitted by this build.
    #[serde(other)]
    Unknown,
}

impl ActivityKind {
    /// Stable wire name for the kind, matching the serde representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            ActivityKind::ModelCall => "model_call",
            ActivityKind::ToolCall => "tool_call",
            ActivityKind::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for ActivityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// AdapterActivity — the serialized heartbeat record
// ──────────────────────────────────────────────────────────────────────────────

/// One adapter-reported activity event, as serialized in the heartbeat JSON.
///
/// Every field except `kind` is bound by NEEDLE, not the adapter: the
/// reporter's API takes only an [`ActivityKind`], and the slot re-stamps
/// `observed_at` at ingest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterActivity {
    /// Identity of the dispatch attempt this activity belongs to (the
    /// provisional UUIDv7 attempt ID minted at dispatch start).
    pub attempt_id: String,
    /// The bead the attempt is working on. Bound by NEEDLE at reporter
    /// creation; a report for any other bead is dropped.
    pub bead_id: BeadId,
    /// Monotonically increasing sequence assigned by the reporter. A record
    /// whose sequence is not strictly newer than the retained one is
    /// dropped, so duplicate and reordered events lose safely.
    pub seq: u64,
    /// What kind of activity the adapter reported.
    pub kind: ActivityKind,
    /// When NEEDLE observed the event — stamped at ingest, never taken from
    /// the caller.
    pub observed_at: DateTime<Utc>,
}

// ──────────────────────────────────────────────────────────────────────────────
// ActivitySlot — the in-memory slot the emitter snapshots
// ──────────────────────────────────────────────────────────────────────────────

/// The activity context of the dispatch in flight plus the latest accepted
/// event.
///
/// Lives inside [`SharedHeartbeatState`], guarded by its mutex. All intake
/// rules are enforced here so there is exactly one place that decides
/// whether an event reaches the heartbeat view.
#[derive(Debug, Default)]
pub struct ActivitySlot {
    /// `(attempt_id, bead_id)` of the dispatch activity is bound to.
    /// `None` when no dispatch is active — every report is dropped.
    bound: Option<(String, BeadId)>,
    /// The latest accepted event, if any.
    latest: Option<AdapterActivity>,
}

impl ActivitySlot {
    /// Bind the slot to a dispatch attempt and reset any previous event.
    ///
    /// Called when a dispatch starts. Resetting (rather than keeping) the
    /// previous attempt's last event means a successor attempt's heartbeat
    /// view starts clean and can only be refreshed by its own reporter.
    pub fn bind(&mut self, attempt_id: String, bead_id: BeadId) {
        if !identifiers_are_bounded(&attempt_id, &bead_id) {
            self.clear();
            return;
        }
        self.bound = Some((attempt_id, bead_id));
        self.latest = None;
    }

    /// Unbind the slot and drop the retained event.
    ///
    /// Called when the dispatch ends, so a late event from a finished
    /// attempt cannot refresh the heartbeat afterwards.
    pub fn clear(&mut self) {
        self.bound = None;
        self.latest = None;
    }

    /// Ingest one reported event, applying the intake rules.
    ///
    /// Returns `true` if the event was accepted into the heartbeat view,
    /// `false` if it was dropped (no live dispatch, stale attempt, wrong
    /// bead, or a sequence that is not strictly newer than the retained
    /// event). Dropped events are lost safely — the retained view is
    /// untouched — and are never an error for the caller.
    ///
    /// The observed time is (re-)stamped here from NEEDLE's clock: the
    /// submitted value, forged or honest, is never trusted, so receive time
    /// is authoritative.
    pub fn ingest(&mut self, mut event: AdapterActivity) -> bool {
        event.observed_at = Utc::now();

        if !identifiers_are_bounded(&event.attempt_id, &event.bead_id) {
            return false;
        }
        let Some((attempt_id, bead_id)) = &self.bound else {
            return false;
        };
        if *attempt_id != event.attempt_id || *bead_id != event.bead_id {
            return false;
        }
        if let Some(latest) = &self.latest {
            if event.seq <= latest.seq {
                return false;
            }
        }
        self.latest = Some(event);
        true
    }

    /// The latest accepted event, if any.
    pub fn latest(&self) -> Option<&AdapterActivity> {
        self.latest.as_ref()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ActivityReporter — the thread-safe handle an adapter integration holds
// ──────────────────────────────────────────────────────────────────────────────

/// Thread-safe reporter for adapter activity, owned by one live dispatch.
///
/// Created by [`crate::health::HealthMonitor::bind_activity`] when a dispatch
/// starts and detached when it ends. Clone it into any thread or callback an
/// adapter integration owns; reports from every clone land in the same
/// attempt-scoped slot.
///
/// Reporting is best-effort by design: a dropped event (stale attempt,
/// wrong bead, out-of-order, poisoned lock) is not an error and must never
/// fail the dispatch, so `report` returns whether the event was accepted
/// rather than a `Result`.
///
/// ```ignore
/// let reporter = health.bind_activity(attempt_id, bead_id);
/// let reporter_for_hook = reporter.clone();
/// // ... inside the adapter integration's tool-call callback:
/// reporter_for_hook.report(ActivityKind::ToolCall);
/// ```
#[derive(Clone)]
pub struct ActivityReporter {
    shared: Arc<Mutex<SharedHeartbeatState>>,
    attempt_id: String,
    bead_id: BeadId,
    next_seq: Arc<AtomicU64>,
}

impl ActivityReporter {
    pub(super) fn new(
        shared: Arc<Mutex<SharedHeartbeatState>>,
        attempt_id: String,
        bead_id: BeadId,
    ) -> Self {
        ActivityReporter {
            shared,
            attempt_id,
            bead_id,
            next_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Report one activity event for the live dispatch.
    ///
    /// Returns `true` if the event was accepted into the heartbeat view.
    /// Events are accepted only while this reporter's attempt is still the
    /// dispatch bound in the heartbeat state, only for the bead it was
    /// created with, and only when the NEEDLE-assigned sequence is strictly
    /// newer than the retained event.
    ///
    /// The adapter supplies nothing but the kind — NEEDLE binds worker,
    /// attempt, bead, sequence, and observed time, and the record carries no
    /// payload field, so there is nothing else an integration could inject.
    pub fn report(&self, kind: ActivityKind) -> bool {
        let event = AdapterActivity {
            attempt_id: self.attempt_id.clone(),
            bead_id: self.bead_id.clone(),
            seq: self.next_seq.fetch_add(1, Ordering::Relaxed),
            kind,
            // Placeholder; the slot re-stamps from NEEDLE's clock at ingest.
            observed_at: Utc::now(),
        };
        match self.shared.lock() {
            Ok(mut guard) => {
                // The bead binding is checked again at receive time. A worker
                // can move to idle or a successor bead before a callback from
                // the old attempt arrives; such a callback must not refresh
                // even if the slot still carries the old attempt identity.
                if guard.current_bead.as_ref() != Some(&self.bead_id) {
                    return false;
                }
                guard.activity.ingest(event)
            }
            // A poisoned lock means a thread panicked mid-update; losing one
            // progress event is strictly better than panicking the caller.
            Err(_) => false,
        }
    }

    /// Report that a model/API call is in progress.
    pub fn report_model_call(&self) -> bool {
        self.report(ActivityKind::ModelCall)
    }

    /// Report that a tool invocation is in progress.
    pub fn report_tool_call(&self) -> bool {
        self.report(ActivityKind::ToolCall)
    }

    /// The attempt identity this reporter is bound to.
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    /// The bead this reporter is bound to.
    pub fn bead_id(&self) -> &BeadId {
        &self.bead_id
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn event(attempt: &str, bead: &str, seq: u64) -> AdapterActivity {
        AdapterActivity {
            attempt_id: attempt.to_string(),
            bead_id: BeadId::from(bead),
            seq,
            kind: ActivityKind::ToolCall,
            // A forged timestamp: the slot must never keep this value.
            observed_at: DateTime::<Utc>::MAX_UTC,
        }
    }

    fn slot_bound_to(attempt: &str, bead: &str) -> ActivitySlot {
        let mut slot = ActivitySlot::default();
        slot.bind(attempt.to_string(), BeadId::from(bead));
        slot
    }

    // ── Ingest rules ────────────────────────────────────────────────────────

    #[test]
    fn ingest_without_a_bound_dispatch_drops_every_event() {
        let mut slot = ActivitySlot::default();
        assert!(!slot.ingest(event("attempt-1", "needle-abc", 0)));
        assert!(slot.latest().is_none());
    }

    #[test]
    fn ingest_accepts_valid_progress_for_the_bound_attempt_and_bead() {
        let mut slot = slot_bound_to("attempt-1", "needle-abc");
        assert!(slot.ingest(event("attempt-1", "needle-abc", 0)));

        let latest = slot.latest().expect("valid progress is retained");
        assert_eq!(latest.attempt_id, "attempt-1");
        assert_eq!(latest.bead_id, BeadId::from("needle-abc"));
        assert_eq!(latest.seq, 0);
        assert_eq!(latest.kind, ActivityKind::ToolCall);
    }

    #[test]
    fn ingest_drops_a_wrong_bead_event() {
        let mut slot = slot_bound_to("attempt-1", "needle-abc");
        assert!(!slot.ingest(event("attempt-1", "needle-other", 0)));
        assert!(slot.latest().is_none());
    }

    #[test]
    fn ingest_drops_a_stale_attempt_event() {
        // A successor attempt is bound; a late event from the prior attempt
        // must lose instead of refreshing the successor's heartbeat.
        let mut slot = slot_bound_to("attempt-2", "needle-abc");
        assert!(!slot.ingest(event("attempt-1", "needle-abc", 999)));
        assert!(slot.latest().is_none());
    }

    #[test]
    fn ingest_drops_duplicate_sequences() {
        let mut slot = slot_bound_to("attempt-1", "needle-abc");
        assert!(slot.ingest(event("attempt-1", "needle-abc", 3)));
        assert!(!slot.ingest(event("attempt-1", "needle-abc", 3)));
        assert_eq!(slot.latest().unwrap().seq, 3);
    }

    #[test]
    fn ingest_drops_reordered_events() {
        let mut slot = slot_bound_to("attempt-1", "needle-abc");
        assert!(slot.ingest(event("attempt-1", "needle-abc", 5)));
        assert!(!slot.ingest(event("attempt-1", "needle-abc", 4)));
        assert_eq!(slot.latest().unwrap().seq, 5);
    }

    #[test]
    fn ingest_re_stamps_the_observed_time_from_needles_clock() {
        let mut slot = slot_bound_to("attempt-1", "needle-abc");
        assert!(slot.ingest(event("attempt-1", "needle-abc", 0)));

        let latest = slot.latest().unwrap();
        // The forged MAX_UTC was replaced by NEEDLE's receive time.
        let skew = (Utc::now() - latest.observed_at).num_milliseconds().abs();
        assert!(
            skew < 60_000,
            "observed_at must be NEEDLE's receive time, got {latest:?}"
        );
    }

    #[test]
    fn clear_rejects_every_late_event() {
        let mut slot = slot_bound_to("attempt-1", "needle-abc");
        assert!(slot.ingest(event("attempt-1", "needle-abc", 0)));
        slot.clear();
        assert!(!slot.ingest(event("attempt-1", "needle-abc", 1)));
        assert!(slot.latest().is_none());
    }

    #[test]
    fn bind_resets_the_retained_event_for_a_successor_attempt() {
        let mut slot = slot_bound_to("attempt-1", "needle-abc");
        assert!(slot.ingest(event("attempt-1", "needle-abc", 7)));
        slot.bind("attempt-2".to_string(), BeadId::from("needle-abc"));
        assert!(
            slot.latest().is_none(),
            "a successor attempt starts with a clean activity view"
        );
    }

    // ── Reporter API ────────────────────────────────────────────────────────

    /// Build a shared heartbeat state and a reporter bound to it.
    fn reporter_on_state() -> (Arc<Mutex<SharedHeartbeatState>>, ActivityReporter) {
        let shared = Arc::new(Mutex::new(SharedHeartbeatState::for_test()));
        shared.lock().unwrap().current_bead = Some(BeadId::from("needle-abc"));
        let reporter = ActivityReporter::new(
            shared.clone(),
            "attempt-1".to_string(),
            BeadId::from("needle-abc"),
        );
        (shared, reporter)
    }

    #[test]
    fn reporter_reports_model_and_tool_activity() {
        let (shared, reporter) = reporter_on_state();
        shared.lock().unwrap().activity.bind(
            reporter.attempt_id().to_string(),
            reporter.bead_id().clone(),
        );

        assert!(reporter.report_model_call());
        assert!(reporter.report_tool_call());

        let guard = shared.lock().unwrap();
        let latest = guard.activity.latest().expect("activity retained");
        assert_eq!(latest.seq, 1, "sequences are assigned in report order");
        assert_eq!(latest.kind, ActivityKind::ToolCall);
    }

    #[test]
    fn reporter_assigns_monotonic_sequences_under_concurrent_reports() {
        let (shared, reporter) = reporter_on_state();
        shared.lock().unwrap().activity.bind(
            reporter.attempt_id().to_string(),
            reporter.bead_id().clone(),
        );

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let reporter = reporter.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        reporter.report_tool_call();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }

        let guard = shared.lock().unwrap();
        let latest = guard.activity.latest().expect("some report was accepted");
        // Whatever survived the dedup race, the retained record is the
        // strictly-newest accepted one and carries no payload beyond the
        // fixed fields.
        assert_eq!(latest.bead_id, BeadId::from("needle-abc"));
        assert_eq!(latest.kind, ActivityKind::ToolCall);
        assert_eq!(latest.attempt_id, "attempt-1");
    }

    #[test]
    fn high_rate_reporting_coalesces_into_one_retained_record() {
        let (shared, reporter) = reporter_on_state();
        shared.lock().unwrap().activity.bind(
            reporter.attempt_id().to_string(),
            reporter.bead_id().clone(),
        );

        for seq in 0..1000 {
            assert!(reporter.report_model_call());
            let guard = shared.lock().unwrap();
            assert_eq!(guard.activity.latest().unwrap().seq, seq);
        }
        // Exactly one record is retained no matter how many were reported —
        // the emitter persists at most this one small record per interval.
    }

    #[test]
    fn reporter_loses_when_the_current_bead_changes() {
        let (shared, reporter) = reporter_on_state();
        shared.lock().unwrap().activity.bind(
            reporter.attempt_id().to_string(),
            reporter.bead_id().clone(),
        );

        assert!(reporter.report_model_call());
        shared.lock().unwrap().current_bead = Some(BeadId::from("needle-successor"));

        assert!(!reporter.report_tool_call());
        assert_eq!(
            shared
                .lock()
                .unwrap()
                .activity
                .latest()
                .expect("the old event remains visible until the next heartbeat boundary")
                .seq,
            0
        );
    }

    #[test]
    fn oversized_identifiers_are_not_retained() {
        let mut slot = ActivitySlot::default();
        let oversized_attempt = "a".repeat(MAX_ACTIVITY_ATTEMPT_ID_LEN + 1);
        let oversized_bead = BeadId::from("b".repeat(MAX_ACTIVITY_BEAD_ID_LEN + 1));
        slot.bind(oversized_attempt.clone(), oversized_bead.clone());
        assert!(!slot.ingest(AdapterActivity {
            attempt_id: oversized_attempt,
            bead_id: oversized_bead,
            seq: 0,
            kind: ActivityKind::ToolCall,
            observed_at: Utc::now(),
        }));
        assert!(slot.latest().is_none());
    }

    // ── Bounded wire shape ──────────────────────────────────────────────────

    #[test]
    fn serialized_record_carries_exactly_the_bounded_field_set() {
        let json = serde_json::to_value(event("attempt-1", "needle-abc", 0)).unwrap();
        let object = json.as_object().expect("record serializes to an object");
        let mut keys: Vec<_> = object.keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["attempt_id", "bead_id", "kind", "observed_at", "seq"],
            "a new field here is a payload channel — extend the bounded rule set first"
        );
    }

    #[test]
    fn kinds_serialize_as_snake_case_and_unknown_kinds_read_safely() {
        assert_eq!(
            serde_json::to_value(ActivityKind::ModelCall).unwrap(),
            serde_json::json!("model_call")
        );
        assert_eq!(
            serde_json::to_value(ActivityKind::ToolCall).unwrap(),
            serde_json::json!("tool_call")
        );
        let parsed: ActivityKind = serde_json::from_value(serde_json::json!("something_newer"))
            .expect("an unknown kind from a newer build must not fail the heartbeat");
        assert_eq!(parsed, ActivityKind::Unknown);
    }
}
