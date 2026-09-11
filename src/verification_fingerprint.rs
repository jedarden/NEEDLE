//! Verification-failure fingerprinting and the cross-bead degradation trip.
//!
//! From 2026-09-01 the clean gate in NEEDLE and commitgraph failed with one
//! identical message — `fatal: not a git repository` — on every bead it ran
//! against: 139 failures across the two workspaces, 40 beads penalised, 9
//! quarantined, and each failure booked against the bead it was judging
//! because the text did not match the runner's ENOENT/EACCES heuristics. A
//! human noticed two days later (N-T22; plan §4.5).
//!
//! The signal that was missed was statistical, not textual: when every
//! verification failure in a workspace carries the same fingerprint, no bead
//! is at fault — the workspace's gate is. This module computes that
//! fingerprint and decides when it has crossed from "a bead failed" to "the
//! gate is broken":
//!
//! - **Fingerprint** = gate name + normalized output. Normalization strips
//!   the things that differ between two runs of the same broken gate — bead
//!   ids, commit hashes, temp paths, timestamps, counts — so two runs of the
//!   same failure collide deliberately and two different failures do not.
//! - **Window** = the failures of one workspace inside a sliding window
//!   (default: the last 2 hours, at most the last 20 failures). The window
//!   must hold at least `min_window_failures` observations before it is
//!   allowed to trip anything: a ratio over three failures is noise, not a
//!   pattern.
//! - **Trip** = one fingerprint covers ≥ `trip_ratio` of the window across ≥
//!   `min_distinct_beads` distinct beads. The distinct-bead requirement is
//!   the whole point: one bead failing five times is a bead problem, five
//!   beads failing identically is an infrastructure problem.
//!
//! The evaluation is a pure function of the window, and the window is
//! pruned against each failure's own timestamp rather than the wall clock, so
//! a recorded telemetry stream replays to the same decisions the live runner
//! made. [`record_verification_failure`] in `gate_health` persists the window
//! next to the consecutive-error state, and the outcome handler turns a trip
//! into exactly the degradation needle-0abc120d already implements.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::time::Duration;

/// Raw gate output is truncated before normalization; a failure log longer
/// than this is not a fingerprint signal, it is a log.
const MAX_RAW_OUTPUT_CHARS: usize = 4096;

/// The normalized text folded into a fingerprint is truncated as well, so two
/// failures that agree on their first `MAX_NORMALIZED_CHARS` characters share
/// a fingerprint instead of diverging on a stack trace tail.
const MAX_NORMALIZED_CHARS: usize = 512;

/// Detector thresholds, mirroring `workspace_health.fingerprint_*` in
/// `NeedleConfig`. Kept as its own plain struct so the decision logic stays
/// replayable without loading a full configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectorConfig {
    /// How far back the sliding window reaches.
    pub window: Duration,
    /// Hard cap on window size, applied after the time prune.
    pub window_max_failures: usize,
    /// Failures that must be in the window before a trip is even evaluated.
    pub min_window_failures: usize,
    /// Share of the window one fingerprint must cover to trip.
    pub trip_ratio: f64,
    /// Distinct beads a dominating fingerprint must span to trip.
    pub min_distinct_beads: usize,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(2 * 60 * 60),
            window_max_failures: 20,
            min_window_failures: 5,
            trip_ratio: 0.80,
            min_distinct_beads: 3,
        }
    }
}

/// One verification failure, reduced to what the detector may see.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationFailure {
    /// When the gate failed.
    pub at: DateTime<Utc>,
    /// The bead the failing gate was judging.
    pub bead: String,
    /// Stable fingerprint of gate name + normalized output.
    pub fingerprint: String,
}

impl VerificationFailure {
    /// Fingerprint and record a verification failure.
    pub fn new(at: DateTime<Utc>, bead: &str, gate: &str, output: &str) -> Self {
        Self {
            at,
            bead: bead.to_string(),
            fingerprint: fingerprint(gate, output),
        }
    }
}

/// The detector's verdict for one recorded failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The window does not (yet) carry an infrastructure signature. The
    /// counts are observability, not a verdict.
    Window {
        /// Failures currently in the window.
        failures: usize,
        /// Beads those failures belong to.
        distinct_beads: usize,
    },
    /// One fingerprint now dominates the window: the workspace is
    /// gate-degraded.
    Tripped {
        /// The fingerprint that dominates the window.
        fingerprint: String,
        /// How many of the window's failures carry it.
        failures: usize,
        /// How many distinct beads those failures belong to.
        distinct_beads: usize,
    },
}

impl Decision {
    /// Whether this decision degrades the workspace.
    pub fn is_trip(&self) -> bool {
        matches!(self, Decision::Tripped { .. })
    }
}

/// What the sliding window held after a record, plus whether it tripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordOutcome {
    /// The verdict for the window as it stands after this failure.
    pub decision: Decision,
    /// The fingerprint of the failure just recorded.
    pub fingerprint: String,
}

/// Prune a window to the failures inside the detector's reach and evaluate it.
///
/// Pure: the same window and config always produce the same decision, which
/// is what lets a recorded telemetry stream be replayed through this function
/// in a test. `now` is the timestamp of the failure being judged, not the
/// wall clock, so replay and live runs agree.
pub fn evaluate(
    window: &[VerificationFailure],
    now: DateTime<Utc>,
    config: &DetectorConfig,
) -> Decision {
    let window_secs = config.window.as_secs() as i64;
    let recent: Vec<&VerificationFailure> = window
        .iter()
        .filter(|f| (now - f.at).num_seconds() <= window_secs)
        .collect();

    // A short window is not evidence of anything. Until it holds
    // `min_window_failures` observations, the newest failure is just the
    // newest failure.
    if recent.len() < config.min_window_failures {
        return Decision::Window {
            failures: recent.len(),
            distinct_beads: distinct_beads(&recent, None),
        };
    }

    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for failure in &recent {
        *counts.entry(failure.fingerprint.as_str()).or_default() += 1;
    }

    // Deterministic winner: highest count, then lexicographically smallest
    // fingerprint, so two fingerprints tied at the threshold produce the same
    // verdict on every worker.
    let top = counts
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(fingerprint, count)| (*fingerprint, *count));

    if let Some((fingerprint, count)) = top {
        let distinct = distinct_beads(&recent, Some(fingerprint));
        let covers = count as f64 / recent.len() as f64;
        if covers >= config.trip_ratio && distinct >= config.min_distinct_beads {
            return Decision::Tripped {
                fingerprint: fingerprint.to_string(),
                failures: count,
                distinct_beads: distinct,
            };
        }
    }

    Decision::Window {
        failures: recent.len(),
        distinct_beads: distinct_beads(&recent, None),
    }
}

/// Count the distinct beads behind a window, optionally only those carrying
/// one fingerprint.
fn distinct_beads(window: &[&VerificationFailure], fingerprint: Option<&str>) -> usize {
    let mut beads: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for failure in window {
        if let Some(expected) = fingerprint {
            if failure.fingerprint != expected {
                continue;
            }
        }
        beads.insert(failure.bead.as_str());
    }
    beads.len()
}

/// Maintain the sliding window for one workspace and evaluate it on every
/// failure. The window is the persistent half — `gate_health` serializes it
/// into the workspace's gate-health state file — while the decision remains
/// the pure [`evaluate`].
#[derive(Debug, Clone)]
pub struct FingerprintTracker {
    window: VecDeque<VerificationFailure>,
    config: DetectorConfig,
}

impl FingerprintTracker {
    /// A tracker starting from a previously persisted window.
    pub fn new(window: Vec<VerificationFailure>, config: DetectorConfig) -> Self {
        let mut window: VecDeque<_> = window.into_iter().collect();
        window.make_contiguous().sort_by_key(|f| f.at);
        Self { window, config }
    }

    /// The failures currently retained (oldest first), for persistence.
    pub fn window(&self) -> Vec<VerificationFailure> {
        self.window.iter().cloned().collect()
    }

    /// Record a failure and judge the window it lands in.
    pub fn record(&mut self, failure: VerificationFailure) -> RecordOutcome {
        // Prune against the failure's own timestamp: replaying a recorded
        // stream must reach the same decisions the live runner reached.
        let window_secs = self.config.window.as_secs() as i64;
        self.window
            .retain(|f| (failure.at - f.at).num_seconds() <= window_secs);
        self.window.push_back(failure.clone());
        while self.window.len() > self.config.window_max_failures {
            self.window.pop_front();
        }

        let decision = evaluate(self.window.make_contiguous(), failure.at, &self.config);
        RecordOutcome {
            decision,
            fingerprint: failure.fingerprint,
        }
    }
}

/// Fingerprint a verification failure: gate name + normalized output.
///
/// The gate name is part of the identity on purpose — the same text coming
/// out of two different gates is two different problems.
pub fn fingerprint(gate: &str, output: &str) -> String {
    let normalized = normalize_output(output);
    let mut hasher = Sha256::new();
    hasher.update(gate.as_bytes());
    hasher.update(b"|");
    hasher.update(normalized.as_bytes());
    let hash = hasher.finalize();
    format!("{:x}", hash)[..12].to_string()
}

/// The bead label carrying a verification fingerprint.
pub fn fingerprint_label(fingerprint: &str) -> String {
    format!("fingerprint:{}", fingerprint)
}

/// Normalize gate output so two runs of the same failure collide.
///
/// Stripped, in order: ISO-8601 timestamps, hex ids and hashes (bead ids,
/// commit SHAs, worker session ids), absolute paths, bare numbers, and
/// whitespace variation. The result is truncated to
/// [`MAX_NORMALIZED_CHARS`] so a failure's stack-trace tail cannot keep two
/// copies of one failure from colliding.
///
/// The order matters: paths are stripped before numbers so the digits inside
/// a path disappear with the path, and hashes before numbers so a 7-character
/// commit SHA is an id rather than two numbers.
pub fn normalize_output(output: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;

    fn compiled(pattern: &str) -> Regex {
        // Unwrapping a literal pattern is the codebase's existing regex
        // convention (see `fingerprint::normalize_cause`).
        Regex::new(pattern).expect("static regex must compile")
    }

    static TIMESTAMP: OnceLock<Regex> = OnceLock::new();
    static HEX_ID: OnceLock<Regex> = OnceLock::new();
    static PATH: OnceLock<Regex> = OnceLock::new();
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    static WHITESPACE: OnceLock<Regex> = OnceLock::new();

    let timestamp = TIMESTAMP.get_or_init(|| {
        compiled(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?")
    });
    let hex_id = HEX_ID.get_or_init(|| compiled(r"\b[0-9a-fA-F]{7,64}\b"));
    // Two or more segments always; a single segment only when it carries a
    // dot, digit, or underscore — so `scripts/definition-of-done.sh`
    // collapses but the slash in "and/or" does not.
    let path = PATH.get_or_init(|| {
        compiled(r"(?:/[A-Za-z0-9._@+-]+){2,}|/[A-Za-z0-9._@+-]*[._0-9][A-Za-z0-9._@+-]*")
    });
    let number = NUMBER.get_or_init(|| compiled(r"\b\d+\b"));
    let whitespace = WHITESPACE.get_or_init(|| compiled(r"\s+"));

    let truncated = &output[..output
        .char_indices()
        .nth(MAX_RAW_OUTPUT_CHARS)
        .map(|(i, _)| i)
        .unwrap_or(output.len())];

    let normalized = timestamp.replace_all(truncated, "<ts>");
    let normalized = hex_id.replace_all(&normalized, "<id>");
    let normalized = path.replace_all(&normalized, "<path>");
    let normalized = number.replace_all(&normalized, "<n>");
    let normalized = whitespace.replace_all(normalized.trim(), " ");

    let mut result = normalized.to_string();
    if result.char_indices().count() > MAX_NORMALIZED_CHARS {
        let cut = result
            .char_indices()
            .nth(MAX_NORMALIZED_CHARS)
            .map(|(i, _)| i)
            .unwrap_or(result.len());
        result.truncate(cut);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_788_000_000 + secs, 0).unwrap()
    }

    fn failure(secs: i64, bead: &str, gate: &str, output: &str) -> VerificationFailure {
        VerificationFailure::new(at(secs), bead, gate, output)
    }

    /// The 2026-09-01 incident output, verbatim.
    const INCIDENT: &str = "command 'scripts/definition-of-done.sh --fast' failed: \
fatal: not a git repository (or any of the parent directories): .git";

    #[test]
    fn identical_gate_abort_collides_across_beads_and_runs() {
        let a = VerificationFailure::new(at(0), "needle-40c6c60e", "gate_1", INCIDENT);
        let b = VerificationFailure::new(at(600), "needle-6a0d3665", "gate_1", INCIDENT);
        assert_eq!(a.fingerprint, b.fingerprint);
    }

    #[test]
    fn different_gates_are_different_fingerprints() {
        let gate = VerificationFailure::new(at(0), "b", "gate_1", INCIDENT);
        let shipped = VerificationFailure::new(at(0), "b", "shipped_work", INCIDENT);
        assert_ne!(gate.fingerprint, shipped.fingerprint);
    }

    #[test]
    fn normalization_strips_ids_hashes_paths_and_numbers() {
        let a = normalize_output(
            "commit 2907849 has substantial changes but has not been pushed to the remote",
        );
        let b = normalize_output(
            "commit 03f8ea1 has substantial changes but has not been pushed to the remote",
        );
        assert_eq!(a, b, "different commit hashes are the same failure shape");
        assert!(a.contains("<id>"), "hash stripped: {a}");

        let pathed_a =
            normalize_output("failed: /tmp/.needle-extract-a1b2/scripts/check.sh exited 3");
        let pathed_b =
            normalize_output("failed: /tmp/.needle-extract-9c8d7e/scripts/check.sh exited 3");
        assert_eq!(pathed_a, pathed_b, "temp paths stripped");
        assert!(!pathed_a.contains('/'), "path gone entirely: {pathed_a}");
    }

    #[test]
    fn normalization_keeps_prose_slashes() {
        // "and/or" is prose, not a path — it must survive so genuinely
        // different messages do not collide.
        let and_or = normalize_output("expected and/or received 5 errors");
        assert!(and_or.contains("and/or"), "{and_or}");
        assert!(and_or.contains("<n>"));
    }

    #[test]
    fn normalization_separates_distinct_failures() {
        let abort = normalize_output(INCIDENT);
        let no_commit = normalize_output(
            "no substantial pushed commit and no bead note recorded for this dispatch",
        );
        assert_ne!(abort, no_commit);
        assert_ne!(
            fingerprint("gate_0", INCIDENT),
            fingerprint("gate_0", "no substantial pushed commit and no bead note"),
        );
    }

    #[test]
    fn window_below_minimum_never_trips() {
        let config = DetectorConfig::default();
        let window: Vec<VerificationFailure> = (0..4)
            .map(|i| failure(i * 60, &format!("bead-{i}"), "gate_0", INCIDENT))
            .collect();
        assert_eq!(
            evaluate(&window, at(300), &config),
            Decision::Window {
                failures: 4,
                distinct_beads: 4
            }
        );
    }

    #[test]
    fn five_identical_failures_across_three_beads_trip() {
        let config = DetectorConfig::default();
        let beads = ["bead-a", "bead-a", "bead-b", "bead-b", "bead-c"];
        let window: Vec<VerificationFailure> = beads
            .iter()
            .enumerate()
            .map(|(i, bead)| failure(i as i64 * 60, bead, "gate_0", INCIDENT))
            .collect();
        assert_eq!(
            evaluate(&window, at(300), &config),
            Decision::Tripped {
                fingerprint: fingerprint("gate_0", INCIDENT),
                failures: 5,
                distinct_beads: 3
            }
        );
    }

    #[test]
    fn one_bead_failing_repeatedly_never_trips() {
        let config = DetectorConfig::default();
        let window: Vec<VerificationFailure> = (0..8)
            .map(|i| failure(i * 60, "same-bead", "gate_0", INCIDENT))
            .collect();
        assert!(!evaluate(&window, at(480), &config).is_trip());
    }

    #[test]
    fn mixed_fingerprints_never_trip() {
        let config = DetectorConfig::default();
        let outputs = [
            INCIDENT,
            "no substantial pushed commit and no bead note recorded for this dispatch",
            "command 'scripts/gate-no-dod-bypass.sh' failed: bypass detected",
        ];
        let beads = ["bead-a", "bead-b", "bead-c", "bead-d", "bead-e"];
        // Round-robin so no fingerprint ever holds more than 2 of 5.
        let window: Vec<VerificationFailure> = beads
            .iter()
            .enumerate()
            .map(|(i, bead)| failure(i as i64 * 60, bead, "gate_0", outputs[i % 3]))
            .collect();
        for now in 0..=10 {
            assert!(
                !evaluate(&window, at(now * 60), &config).is_trip(),
                "mixed window tripped at t+{}m",
                now
            );
        }
    }

    #[test]
    fn a_dominant_minority_below_the_ratio_does_not_trip() {
        let config = DetectorConfig::default();
        let outputs = [
            INCIDENT,
            INCIDENT,
            INCIDENT,
            INCIDENT,
            "other failure entirely",
        ];
        let window: Vec<VerificationFailure> = outputs
            .iter()
            .enumerate()
            .map(|(i, out)| failure(i as i64 * 60, &format!("bead-{i}"), "gate_0", out))
            .collect();
        // 4 of 5 = 80% — exactly at the ratio, so this trips. One fewer and
        // it must not.
        assert!(evaluate(&window, at(300), &config).is_trip());

        let outputs = [INCIDENT, INCIDENT, INCIDENT, "x", "y", "z", "w", "v"];
        let window: Vec<VerificationFailure> = outputs
            .iter()
            .enumerate()
            .map(|(i, out)| failure(i as i64 * 60, &format!("bead-{i}"), "gate_0", out))
            .collect();
        assert!(
            !evaluate(&window, at(480), &config).is_trip(),
            "3 of 8 is not dominance"
        );
    }

    #[test]
    fn failures_older_than_the_window_do_not_count() {
        let config = DetectorConfig::default();
        let window: Vec<VerificationFailure> = (0..5)
            .map(|i| failure(i * 60, &format!("bead-{i}"), "gate_0", INCIDENT))
            .collect();
        // Three hours later only stale failures remain, and the window
        // empties back below the minimum.
        let decision = evaluate(&window, at(3 * 3600), &config);
        assert_eq!(
            decision,
            Decision::Window {
                failures: 0,
                distinct_beads: 0
            }
        );
    }

    #[test]
    fn tracker_prunes_by_time_and_count() {
        let mut tracker = FingerprintTracker::new(Vec::new(), DetectorConfig::default());
        for i in 0..25 {
            tracker.record(failure(i * 60, &format!("bead-{i}"), "gate_0", INCIDENT));
        }
        assert_eq!(
            tracker.window().len(),
            DetectorConfig::default().window_max_failures
        );
        // The oldest retained failure is the one 19 minutes back, not the
        // first one recorded.
        let oldest = tracker.window().first().expect("non-empty").at;
        assert_eq!(oldest, at(6 * 60));
    }

    #[test]
    fn tracker_replays_a_recorded_stream_to_the_same_verdict() {
        let config = DetectorConfig::default();
        let mut tracker = FingerprintTracker::new(Vec::new(), config);
        let beads = ["bead-a", "bead-b", "bead-c", "bead-d", "bead-e"];

        let mut tripped_at = None;
        for (i, bead) in beads.iter().enumerate() {
            let outcome = tracker.record(failure(i as i64 * 60, bead, "gate_0", INCIDENT));
            if outcome.decision.is_trip() && tripped_at.is_none() {
                tripped_at = Some(i + 1);
            }
        }
        assert_eq!(
            tripped_at,
            Some(5),
            "the incident trips on the fifth failure"
        );

        // Replaying the retained window through a fresh tracker must reach
        // the same place.
        let window = tracker.window();
        let mut replay = FingerprintTracker::new(window.clone(), config);
        let outcome = replay.record(failure(300, "bead-f", "gate_0", INCIDENT));
        assert!(outcome.decision.is_trip());
    }

    #[test]
    fn tie_between_two_fingerprints_is_broken_deterministically() {
        let config = DetectorConfig::default();
        let other = "a genuinely different failure";
        let outputs = [INCIDENT, other, INCIDENT, other, "third kind of failure"];
        let window: Vec<VerificationFailure> = outputs
            .iter()
            .enumerate()
            .map(|(i, out)| failure(i as i64 * 60, &format!("bead-{i}"), "gate_0", out))
            .collect();
        // Two fingerprints tied at 2 of 5 — no winner, no trip, whatever the
        // hash order.
        assert!(!evaluate(&window, at(300), &config).is_trip());
    }

    #[test]
    fn fingerprint_label_is_stable_and_short() {
        let fp = fingerprint("gate_0", INCIDENT);
        assert_eq!(fp.len(), 12);
        assert_eq!(fingerprint_label(&fp), format!("fingerprint:{}", fp));
    }

    #[test]
    fn long_outputs_are_truncated_before_hashing() {
        let tail_a = "a".repeat(600);
        let tail_b = "b".repeat(600);
        let prefix = "fatal: not a git repository (or any of the parent directories): .git ";
        assert_eq!(
            fingerprint("gate_0", &format!("{prefix}{tail_a}")),
            fingerprint("gate_0", &format!("{prefix}{tail_b}")),
            "two failures agreeing on their first 512 normalized chars collide"
        );
    }
}
