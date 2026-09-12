//! Replay of the 2026-09-01/02 verification-failure telemetry through the
//! fingerprint detector (N-T22).
//!
//! The incident: from 2026-09-01 the clean gate in NEEDLE and commitgraph
//! failed with one identical message before running a single check — 139
//! failures across the two workspaces — and every one was booked against the
//! bead it was judging, because the text matched neither the ENOENT nor the
//! EACCES heuristic. Nine beads reached quarantine before a human noticed.
//!
//! The detector must reach the verdict a human reached, from the telemetry
//! the runner already had, and it must not reach it for a workspace whose
//! failures genuinely differ from bead to bead. Fixtures are extracts of the
//! real telemetry; see `tests/fixtures/README-verification-failures.md` for
//! exactly what each one contains and what was selected.

use needle::verification_fingerprint::{DetectorConfig, FingerprintTracker};
use std::path::PathBuf;

/// One `verification.failed` telemetry event, reduced to what the detector
/// consumes.
#[derive(serde::Deserialize)]
struct FailureEvent {
    timestamp: String,
    bead_id: String,
    data: FailureData,
}

#[derive(serde::Deserialize)]
struct FailureData {
    command: String,
    output: String,
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Replay a fixture's events, in file order, through a fresh tracker.
///
/// Returns the failure number (1-based) that tripped degradation, if any.
fn replay(name: &str) -> Option<usize> {
    let content = std::fs::read_to_string(fixture(name))
        .unwrap_or_else(|e| panic!("read fixture {name}: {e}"));

    let mut tracker = FingerprintTracker::new(Vec::new(), DetectorConfig::default());
    let mut tripped_at = None;

    for (index, line) in content.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let event: FailureEvent = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("parse fixture {name} line {}: {e}", index + 1));
        let at = chrono::DateTime::parse_from_rfc3339(&event.timestamp)
            .unwrap_or_else(|e| panic!("parse timestamp in {name} line {}: {e}", index + 1))
            .with_timezone(&chrono::Utc);

        let failure = needle::verification_fingerprint::VerificationFailure::new(
            at,
            &event.bead_id,
            &event.data.command,
            &event.data.output,
        );

        let outcome = tracker.record(failure);
        if outcome.decision.is_trip() && tripped_at.is_none() {
            tripped_at = Some(index + 1);
        }
    }

    tripped_at
}

#[test]
fn commitgraph_incident_trips_on_the_fifth_failure() {
    // The real first five failures: four distinct beads, one fingerprint,
    // twenty-one minutes.
    assert_eq!(
        replay("verification-failures-commitgraph-2026-09-01.jsonl"),
        Some(5),
        "the detector must trip within the first five failures"
    );
}

#[test]
fn needle_incident_trips_on_the_fifth_failure() {
    // Five NEEDLE failures spanning three distinct beads inside one window.
    assert_eq!(
        replay("verification-failures-needle-2026-09-01.jsonl"),
        Some(5),
        "the detector must trip within the first five failures"
    );
}

#[test]
fn mixed_fingerprint_workspace_never_trips() {
    // aide-de-camp, 09-01, verbatim: four fingerprints across six beads.
    assert_eq!(
        replay("verification-failures-aide-de-camp-2026-09-01.jsonl"),
        None,
        "a mixed workspace is beads failing, not a gate breaking"
    );
}

#[test]
fn densely_interleaved_fingerprints_never_trip() {
    // Real SEAM outputs alternating across five beads inside one window:
    // density must not turn a mixed stream into an infrastructure verdict.
    assert_eq!(replay("verification-failures-mixed-dense.jsonl"), None);
}
