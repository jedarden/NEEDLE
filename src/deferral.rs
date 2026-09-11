//! Timeout-deferral policy shared by outcome handling, stores, selection, and doctor.

use chrono::{DateTime, Duration, Utc};

use crate::types::Bead;

const BASE_SECS: u64 = 2 * 60 * 60;
const MAX_SECS: u64 = 8 * 60 * 60;

/// Parse an automatic `deferred:<rfc3339>` label.
///
/// A malformed timestamp is treated as absent so bad generated data cannot
/// starve work forever. The bare `deferred` label is deliberately not matched:
/// it is a permanent, operator-owned hold.
pub(crate) fn until(label: &str) -> Option<DateTime<Utc>> {
    const PREFIX: &str = "deferred:";
    let trimmed = label.trim();
    if trimmed.len() < PREFIX.len() || !trimmed[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    DateTime::parse_from_rfc3339(trimmed[PREFIX.len()..].trim())
        .ok()
        .map(|instant| instant.with_timezone(&Utc))
}

/// Return the latest active automatic deferral on a bead.
pub(crate) fn active_until(bead: &Bead, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    bead.labels
        .iter()
        .filter_map(|label| until(label))
        .max()
        .filter(|instant| *instant > now)
}

/// Whether one label excludes a bead at `now`.
pub(crate) fn holds(label: &str, now: DateTime<Utc>) -> bool {
    let trimmed = label.trim();
    trimmed.eq_ignore_ascii_case("deferred") || until(trimmed).is_some_and(|instant| instant > now)
}

/// Whether the bead carries the permanent, operator-owned form.
pub(crate) fn is_permanent(bead: &Bead) -> bool {
    bead.labels
        .iter()
        .any(|label| label.trim().eq_ignore_ascii_case("deferred"))
}

/// The length of the automatic hold for a 1-based consecutive-failure round.
///
/// The ladder is two hours, four hours, then eight hours for every later round.
pub(crate) fn window_for_round(round: u32) -> Duration {
    let exponent = round.saturating_sub(1).min(20);
    let multiplier = 1u64.checked_shl(exponent).unwrap_or(u64::MAX);
    Duration::seconds(BASE_SECS.saturating_mul(multiplier).min(MAX_SECS) as i64)
}

/// Build the automatic timeout hold for a 1-based consecutive-failure round.
///
/// The ladder is two hours, four hours, then eight hours for every later round.
pub(crate) fn label_for_round(round: u32, now: DateTime<Utc>) -> String {
    format!("deferred:{}", (now + window_for_round(round)).to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn automatic_deferral_holds_only_until_its_timestamp() {
        let now = Utc.with_ymd_and_hms(2026, 9, 10, 0, 0, 0).unwrap();
        assert!(holds("deferred", now));
        assert!(holds("deferred:2026-09-10T00:00:01Z", now));
        assert!(!holds("deferred:2026-09-09T23:59:59Z", now));
        assert!(!holds("deferred:not-a-timestamp", now));
    }

    #[test]
    fn automatic_deferral_ladder_expires_and_caps_at_eight_hours() {
        let now = Utc.with_ymd_and_hms(2026, 9, 10, 0, 0, 0).unwrap();
        assert_eq!(
            label_for_round(1, now),
            "deferred:2026-09-10T02:00:00+00:00"
        );
        assert_eq!(
            label_for_round(2, now),
            "deferred:2026-09-10T04:00:00+00:00"
        );
        assert_eq!(
            label_for_round(99, now),
            "deferred:2026-09-10T08:00:00+00:00"
        );
    }
}
