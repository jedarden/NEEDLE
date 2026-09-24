//! Classification of beads produced by NEEDLE's control plane.
//!
//! A control-plane bead is a diagnostic or generator artifact, not work for a
//! target repository.  Keep this predicate in one module: every strand that
//! selects a bead as prompt input or dispatch work must use it before doing so.

use crate::types::Bead;

/// Labels reserved for NEEDLE-owned diagnostics and generated artifacts.
///
/// These labels are intentionally conservative.  A bead carrying one is
/// already inside the control-plane boundary even when it also carries the
/// `human` label or has otherwise become claimable.
pub const INTERNAL_LABELS: &[&str] = &[
    "alert",
    "crash",
    "starvation",
    "knot-starvation",
    "pluck-starvation",
    "gate-broken",
    "generation-ratio",
    "pulse-finding",
    "unravel-proposal",
    "worker-failure",
    "worker-loop",
    "infra",
];

/// Exact body headings emitted by NEEDLE's diagnostic producers.
const INTERNAL_BODY_MARKERS: &[&str] = &[
    "## agent crash report",
    "## gate execution error",
    "## worker failure",
    "## live worker loop detected",
    "## scanner finding",
    "## alternative for:",
];

/// Return whether a label is reserved for a NEEDLE control-plane artifact.
pub fn is_internal_label(label: &str) -> bool {
    let normalized = label.trim().to_ascii_lowercase();
    INTERNAL_LABELS.contains(&normalized.as_str()) || normalized.starts_with("fingerprint:")
}

/// Classify a bead as a NEEDLE-owned control-plane artifact.
///
/// The order is deliberate:
///
/// 1. Structured labels are authoritative and survive title/body edits.
/// 2. Exact generated-body headings cover records whose labels were lost.
/// 3. Historical content matching is a conservative compatibility fallback.
///    It requires diagnostic evidence in the body; a title by itself is never
///    enough for the alert-shaped fallback.
pub fn is_internal_artifact(bead: &Bead) -> bool {
    bead.labels.iter().any(|label| is_internal_label(label))
        || body_has_internal_marker(bead.body.as_deref())
        || historical_diagnostic_shape(&bead.title, bead.body.as_deref())
}

/// Explain the first matching classification layer for structured telemetry or
/// diagnostics.  The returned values are stable machine-readable categories.
pub fn internal_artifact_reason(bead: &Bead) -> Option<&'static str> {
    if bead.labels.iter().any(|label| is_internal_label(label)) {
        return Some("internal_label");
    }
    if body_has_internal_marker(bead.body.as_deref()) {
        return Some("internal_body_marker");
    }
    if historical_diagnostic_shape(&bead.title, bead.body.as_deref()) {
        return Some("historical_diagnostic_shape");
    }
    None
}

fn body_has_internal_marker(body: Option<&str>) -> bool {
    let body = body.unwrap_or_default().to_ascii_lowercase();
    INTERNAL_BODY_MARKERS
        .iter()
        .any(|marker| body.contains(marker))
}

fn historical_diagnostic_shape(title: &str, body: Option<&str>) -> bool {
    let body = body.unwrap_or_default().to_ascii_lowercase();
    if body.trim().is_empty() {
        return false;
    }

    let title = title.to_ascii_lowercase();

    // The original empty-workspace starvation alert predates reserved labels.
    // Require both its alert-shaped title and diagnostic context in the body,
    // so an ordinary bead with a similar title remains eligible.
    let starvation_title = title.contains("starvation alert")
        || title.contains("beads invisible to worker")
        || title.contains("open beads exist but pluck found none");
    let starvation_context = [
        "workspace",
        "open beads",
        "ready beads",
        "pluck",
        "candidate",
        "invisible",
        "starvation",
    ]
    .iter()
    .any(|marker| body.contains(marker));
    if starvation_title && starvation_context {
        return true;
    }

    // Preserve the pre-classification NEEDLE configuration boundary used by
    // Mitosis.  These are source-material records about the scheduler itself,
    // not target-repository implementation work.  The body requirement keeps
    // title-only alert text from becoming a classifier.
    let internal_config_terms = [
        "pluck configuration",
        "pluck config",
        "exclude_labels",
        "exclude labels",
        "bead discovery",
        "needle dispatch",
        "strand configuration",
        "worker configuration",
        "bead filtering",
        "candidate exclusion",
    ];
    if internal_config_terms.iter().any(|term| body.contains(term)) {
        return true;
    }

    // Unlabelled legacy Unravel children and diagnostic proposals can still be
    // recognized from the body they generated.
    body.contains("automated alternative proposal")
        || (title.starts_with("[unravel]") && body.contains("original bead:"))
}
