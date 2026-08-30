//! Alert bead fingerprinting and deduplication.
//!
//! Prevents duplicate alert beads by computing a stable fingerprint
//! from workspace+kind+normalized cause and checking for existing alerts
//! before creating new beads.

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};

use crate::bead_store::BeadStore;
use crate::types::{Bead, BeadId, BeadStatus};

/// Alert bead kind identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AlertKind {
    /// Knot strand starvation alert (exhaustion without claimable beads).
    KnotStarvation,
    /// Pluck strand starvation alert (beads invisible to selector).
    PluckStarvation,
    /// Agent crash alert (signal or exit code).
    Crash,
    /// Unravel proposal alert (alternative deliverables).
    UnravelProposal,
    /// Pulse finding alert (pattern scanner detection).
    PulseFinding,
    /// Gate-broken alert (dependency cycle blocking progress).
    GateBroken,
    /// Generation ratio alert (backlog growing for consecutive days).
    GenerationRatio,
}

impl AlertKind {
    /// String identifier for fingerprint computation.
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertKind::KnotStarvation => "knot-starvation",
            AlertKind::PluckStarvation => "pluck-starvation",
            AlertKind::Crash => "crash",
            AlertKind::UnravelProposal => "unravel-proposal",
            AlertKind::PulseFinding => "pulse-finding",
            AlertKind::GateBroken => "gate-broken",
            AlertKind::GenerationRatio => "generation-ratio",
        }
    }
}

/// Alert deduplication result.
#[derive(Debug)]
pub enum AlertDeduplication {
    /// No duplicate found - safe to create new bead.
    CreateNew,
    /// Duplicate open bead exists - timestamped note was appended.
    Deduplicated {
        /// The existing bead ID.
        bead_id: BeadId,
        /// The fingerprint label.
        fingerprint: String,
    },
    /// Bead was closed within the suppression window - don't create.
    Suppressed {
        /// The closed bead ID.
        bead_id: BeadId,
        /// When it was closed.
        closed_at: DateTime<Utc>,
    },
}

/// Compute a fingerprint label from workspace, kind, and cause.
///
/// The fingerprint is the first 12 hex characters of the SHA-256 hash
/// of `workspace + ":" + kind + ":" + normalized_cause`.
///
/// This provides collision resistance while keeping labels short.
pub fn compute_fingerprint(workspace: &str, kind: &AlertKind, cause: &str) -> String {
    let normalized = normalize_cause(cause);
    let input = format!("{}:{}:{}", workspace, kind.as_str(), normalized);

    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let hash = hasher.finalize();

    // Take first 12 characters (6 bytes) of hex hash
    format!("fingerprint:{:12x}", hash)
}

/// Normalize cause text for fingerprint computation.
///
/// Normalization removes:
/// - Timestamps (ISO 8601 dates)
/// - Hex IDs (bead IDs, worker IDs)
/// - Counts and numeric metrics
/// - Whitespace variations
///
/// This ensures that semantically equivalent causes produce the same fingerprint.
fn normalize_cause(cause: &str) -> String {
    let mut normalized = cause.to_string();

    // Remove ISO 8601 timestamps (e.g., "2024-08-26T15:30:45Z")
    normalized = regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z?")
        .unwrap()
        .replace_all(&normalized, "<timestamp>")
        .to_string();

    // Remove hex IDs (e.g., "abc123def", "needle-abc123")
    normalized = regex::Regex::new(r"\b[a-f0-9]{8,}\b")
        .unwrap()
        .replace_all(&normalized, "<id>")
        .to_string();

    // Remove numeric metrics (e.g., "count=42", "15 open")
    normalized = regex::Regex::new(r"\b\d+\b")
        .unwrap()
        .replace_all(&normalized, "<n>")
        .to_string();

    // Normalize whitespace
    normalized = regex::Regex::new(r"\s+")
        .unwrap()
        .replace_all(normalized.trim(), " ")
        .to_string();

    normalized
}

/// Check for existing alert beads with the given fingerprint.
///
/// Returns a deduplication result indicating whether to create a new bead,
/// update an existing one, or suppress creation entirely.
pub async fn check_alert_deduplication(
    store: &dyn BeadStore,
    workspace: &str,
    kind: &AlertKind,
    cause: &str,
) -> Result<AlertDeduplication> {
    let fingerprint = compute_fingerprint(workspace, kind, cause);

    // Query all beads to find ones with the fingerprint label
    let all_beads = store.list_all().await?;

    // Find open beads with the fingerprint label
    let open_beads: Vec<&Bead> = all_beads
        .iter()
        .filter(|b| b.status == BeadStatus::Open && b.labels.iter().any(|l| l == &fingerprint))
        .collect();

    // Find recently closed beads (within 24h)
    let suppression_window = Utc::now() - Duration::hours(24);
    let recently_closed: Vec<&Bead> = all_beads
        .iter()
        .filter(|b| {
            b.status == BeadStatus::Closed
                && b.updated_at > suppression_window
                && b.labels.iter().any(|l| l == &fingerprint)
        })
        .collect();

    // Priority: suppress if recently closed, deduplicate if open, otherwise create new
    if let Some(closed) = recently_closed.first() {
        return Ok(AlertDeduplication::Suppressed {
            bead_id: closed.id.clone(),
            closed_at: closed.updated_at,
        });
    }

    if let Some(open) = open_beads.first() {
        return Ok(AlertDeduplication::Deduplicated {
            bead_id: open.id.clone(),
            fingerprint,
        });
    }

    Ok(AlertDeduplication::CreateNew)
}

/// Append a timestamped note to an existing bead's notes.
///
/// This is used when deduplicating an alert - each occurrence gets logged
/// to the same bead instead of creating duplicates.
pub async fn append_alert_note(
    _store: &dyn BeadStore,
    bead_id: &BeadId,
    message: &str,
) -> Result<()> {
    let timestamp = Utc::now().to_rfc3339();
    let note = format!("[{}] {}", timestamp, message);

    // Note: BeadStore doesn't currently support adding notes/comments.
    // We log the note and emit telemetry instead.
    tracing::info!(
        bead_id = %bead_id,
        "Would append note to bead: {}", note
    );

    // TODO: Add BeadStore::add_note method and call it here
    Ok(())
}

/// Build fingerprint labels for a new alert bead.
///
/// Returns a vector of labels including the fingerprint label and any
/// additional labels needed for the alert type.
pub fn build_alert_labels(fingerprint: &str, additional_labels: &[&str]) -> Vec<String> {
    let mut labels = vec![fingerprint.to_string()];
    labels.extend(additional_labels.iter().map(|s| s.to_string()));
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_fingerprint() {
        let fp1 = compute_fingerprint(
            "/home/coding/icg",
            &AlertKind::PluckStarvation,
            "cause text",
        );
        let fp2 = compute_fingerprint(
            "/home/coding/icg",
            &AlertKind::PluckStarvation,
            "cause text",
        );
        let fp3 = compute_fingerprint(
            "/home/coding/icg",
            &AlertKind::PluckStarvation,
            "different cause",
        );

        // Same inputs produce same fingerprint
        assert_eq!(fp1, fp2);

        // Different causes produce different fingerprints
        assert_ne!(fp1, fp3);

        // Fingerprint format is correct
        assert!(fp1.starts_with("fingerprint:"));
        assert_eq!(fp1.len(), "fingerprint:".len() + 12);
    }

    #[test]
    fn test_normalize_cause() {
        let cause1 = "2024-08-26T15:30:45Z bead abc123def456 count=42 open 15";
        let cause2 = "2024-08-27T16:31:46Z bead fed987cba12 count=99 open 20";
        let normalized1 = normalize_cause(cause1);
        let normalized2 = normalize_cause(cause2);

        // After normalization, timestamps and IDs are removed
        assert!(normalized1.contains("<timestamp>"));
        assert!(normalized1.contains("<id>"));
        assert!(normalized1.contains("<n>"));

        // Different causes with same pattern normalize to same result
        assert_eq!(normalized1, normalized2);
    }

    #[test]
    fn test_alert_kind_display() {
        assert_eq!(AlertKind::KnotStarvation.as_str(), "knot-starvation");
        assert_eq!(AlertKind::PluckStarvation.as_str(), "pluck-starvation");
        assert_eq!(AlertKind::Crash.as_str(), "crash");
        assert_eq!(AlertKind::UnravelProposal.as_str(), "unravel-proposal");
        assert_eq!(AlertKind::PulseFinding.as_str(), "pulse-finding");
        assert_eq!(AlertKind::GateBroken.as_str(), "gate-broken");
    }

    #[test]
    fn test_build_alert_labels() {
        let fp = "fingerprint:abc123def456";
        let labels = build_alert_labels(fp, &["alert", "starvation"]);

        assert!(labels.contains(&fp.to_string()));
        assert!(labels.contains(&"alert".to_string()));
        assert!(labels.contains(&"starvation".to_string()));
    }
}
