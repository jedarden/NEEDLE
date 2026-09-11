//! Quarantine-expiry re-evaluation: an expired window is a question, not a
//! verdict.
//!
//! Found while fixing needle-ee024ae4 (needle-73d94360): every bead driving a
//! selection livelock carried `quarantine-until` labels whose timestamps had
//! already passed, and each rejoined the pool *unchanged* — same content,
//! `failure-count:3`, `verification-failed` — so the churn quarantine exists
//! to stop resumed the moment the window lapsed. A quarantine that expires on
//! wall clock alone, with no re-evaluation of why it was quarantined, is a
//! delay rather than a guard: it converts a permanent failure into a periodic
//! one.
//!
//! The contract: the expiry moment re-checks the conditions that produced the
//! quarantine, before a dispatch is spent on a known-failing bead.
//! [`evaluate`] is that decision, applied by Pluck at the quarantine filter:
//!
//! - **Conditions cleared → release.** The failure counter fell back below
//!   the threshold (a success reset it, or a degraded-window undo removed
//!   it), the bead's content changed since it was quarantined (someone edited
//!   the work — the fix deserves its attempt), or the evidence is missing
//!   entirely. The bead rejoins the pool.
//! - **Conditions still hold → re-quarantine, never re-dispatch.** The
//!   counter is still at or above the threshold and the content is unchanged
//!   since quarantine, so the bead is re-quarantined behind the next round's
//!   window (2h → 4h → 8h, capped at 48h — ADR-022) without spending a
//!   dispatch on it.
//! - **Ladder exhausted → park, don't cycle.** A bead already at the last
//!   quarantine round whose conditions still hold has exhausted the retry
//!   ladder. It is parked behind an automatic `deferred:` window
//!   ([`crate::deferral`], the reversible-parking form) that renews only
//!   while the conditions keep holding. The park is self-lifting: an edit
//!   changes the content hash, a success resets the counter, and the next
//!   expiry check releases the bead. Rungs 4 and 5 (analysis, human) stay
//!   ahead of it — the park keeps a permanently-stuck bead out of the
//!   frontier without manufacturing the conclusion those rungs exist to
//!   reach.
//!
//! Failing open is deliberate and uniform: every missing or malformed piece
//! of evidence (no `failure-count`, no content hash from a quarantine taken
//! before this module existed, an unparseable window) releases the bead,
//! matching the malformed-label rule of the `quarantine-until` parser. A
//! quarantine must never become a silent starvation mechanism.
//!
//! Depends on: `types`, `bead_store` (label parsing), `deferral`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::bead_store::quarantine_until;
use crate::bead_store::BeadStore;
use crate::types::Bead;

/// ADR-022 quarantine windows. A full quarantine begins at two hours and
/// doubles by round, capped at two days.
pub(crate) const QUARANTINE_BASE_SECS: u64 = 2 * 60 * 60;
pub(crate) const QUARANTINE_MAX_SECS: u64 = 48 * 60 * 60;

/// The last round of the ADR-022 quarantine ladder. Past this round the
/// ladder's next steps belong to the analysis dispatch and the human, not to
/// longer windows — an expired round-3 bead whose conditions still hold is
/// parked, never silently rolled into a round 4.
pub(crate) const LADDER_LAST_ROUND: u32 = 3;

/// Label prefix carrying the content hash a bead was quarantined with.
///
/// The `quarantine:` prefix (not `quarantine-`) is deliberate: it puts the
/// label in the family `OutcomeHandler::reset_failure_count` already clears
/// on success, so a bead that finally ships never drags a stale hash behind
/// it.
pub const CONTENT_HASH_LABEL_PREFIX: &str = "quarantine:content-hash:";

/// Exponential backoff with a cap: `base * 2^exponent`, saturating.
///
/// Shared by the retry cooldown, the quarantine ladder, and the
/// re-quarantine below, so every window in the failure machinery grows on
/// the same curve.
pub(crate) fn capped_exponential_backoff(base_secs: u64, exponent: u32, cap_secs: u64) -> u64 {
    let multiplier = 1u64.checked_shl(exponent.min(20)).unwrap_or(u64::MAX);
    base_secs.saturating_mul(multiplier).min(cap_secs)
}

/// Hash the content a bead was quarantined with.
///
/// Title and body only — the work the bead describes. Labels, assignee and
/// status churn constantly and must not read as "someone fixed it". The
/// digest is truncated to 12 hex chars so it stays readable in a label list;
/// a collision there decides at worst whether to retry work early.
pub fn bead_content_hash(bead: &Bead) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bead.title.as_bytes());
    // Unit separator, so "title" and "title<body" hash differently.
    hasher.update([0x1f]);
    hasher.update(bead.body.as_deref().unwrap_or("").as_bytes());
    let hash = hasher.finalize();
    format!("{:x}", hash)[..12].to_string()
}

/// The label recording what a bead's content was when it was quarantined.
pub fn content_hash_label(bead: &Bead) -> String {
    format!("{CONTENT_HASH_LABEL_PREFIX}{}", bead_content_hash(bead))
}

/// What an expired quarantine window means for a bead at `now`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineExpiry {
    /// No `quarantine-until` label at all — the check does not apply.
    NoWindow,
    /// The latest window is still in the future — the hold stands.
    Active,
    /// The window expired and the quarantine's conditions no longer hold —
    /// the bead rejoins the pool.
    Expired,
    /// The window expired and every condition that caused the quarantine
    /// still holds — re-quarantine behind the next round's window instead of
    /// re-dispatching the bead.
    StillHolding {
        /// The bead's current `quarantine-round` (0 when a round label is
        /// missing — an old or hand-made quarantine starts the ladder fresh).
        round: u32,
        /// The `failure-count` still on the bead.
        failure_count: u32,
    },
}

/// Re-evaluate a bead's quarantine at `now` against the failure `threshold`
/// that triggers one.
///
/// Pure: reads only the bead's labels and content, so every worker reaches
/// the same verdict and a test can replay it. Callers apply this at selection
/// time, on beads that are open and unassigned — a bead someone holds or that
/// is not open is not a selection candidate regardless of its quarantine.
pub fn evaluate(bead: &Bead, now: DateTime<Utc>, threshold: u32) -> QuarantineExpiry {
    if crate::bead_store::active_quarantine_until(bead, now).is_some() {
        return QuarantineExpiry::Active;
    }
    if !bead
        .labels
        .iter()
        .any(|label| quarantine_until(label).is_some())
    {
        return QuarantineExpiry::NoWindow;
    }

    // The window has expired. A disabled threshold (0) is "everything
    // cleared" by definition — the mechanism is off.
    if threshold == 0 {
        return QuarantineExpiry::Expired;
    }

    // The counter is the surviving evidence of the quarantine's cause: only
    // a success (reset_failure_count) or a degraded-window undo removes it.
    // Without it there is nothing saying the bead still fails.
    let failure_count = bead
        .labels
        .iter()
        .filter_map(|label| label.strip_prefix("failure-count:"))
        .filter_map(|count| count.trim().parse::<u32>().ok())
        .max();
    let Some(failure_count) = failure_count else {
        return QuarantineExpiry::Expired;
    };
    if failure_count < threshold {
        return QuarantineExpiry::Expired;
    }

    // The content hash from quarantine time is the "nothing was fixed"
    // check. Absent — a quarantine taken before this module existed — reads
    // as cleared: missing evidence fails open, the bead is attempted once,
    // and its next quarantine records a hash the check can use.
    let recorded = bead
        .labels
        .iter()
        .filter_map(|label| label.strip_prefix(CONTENT_HASH_LABEL_PREFIX))
        .max();
    match recorded {
        None => QuarantineExpiry::Expired,
        Some(recorded) if recorded != bead_content_hash(bead) => QuarantineExpiry::Expired,
        Some(_) => QuarantineExpiry::StillHolding {
            round: quarantine_round_of(bead),
            failure_count,
        },
    }
}

/// The bead's highest `quarantine-round:N`, or 0 when it carries none.
fn quarantine_round_of(bead: &Bead) -> u32 {
    bead.labels
        .iter()
        .filter_map(|label| label.strip_prefix("quarantine-round:"))
        .filter_map(|round| round.trim().parse::<u32>().ok())
        .max()
        .unwrap_or(0)
}

/// Re-quarantine a bead whose expired window's conditions still hold, at the
/// next ladder round.
///
/// Mirrors the outcome handler's `quarantine_bead` ordering: the new window
/// is written before the stale ones are pruned, so a backend failure midway
/// leaves the bead covered by either window rather than briefly back on the
/// frontier. Returns the round and expiry actually written.
pub(crate) async fn requarantine(
    store: &dyn BeadStore,
    bead: &Bead,
    current_round: u32,
    failure_count: u32,
) -> Result<(u32, DateTime<Utc>)> {
    let labels = store
        .labels(&bead.id)
        .await
        .context("labels() timed out while re-quarantining an expired bead")?;
    let round = current_round.saturating_add(1);
    let window_secs = capped_exponential_backoff(
        QUARANTINE_BASE_SECS,
        round.saturating_sub(1),
        QUARANTINE_MAX_SECS,
    );
    let until = Utc::now() + chrono::Duration::seconds(window_secs as i64);
    let round_label = format!("quarantine-round:{round}");
    let until_label = format!("quarantine-until:{}", until.to_rfc3339());
    let hash_label = content_hash_label(bead);

    for label in [
        "quarantined".to_string(),
        round_label.clone(),
        until_label.clone(),
        format!("quarantine:failure-count:{failure_count}"),
        hash_label.clone(),
    ] {
        store
            .add_label(&bead.id, &label)
            .await
            .with_context(|| format!("add_label({label}) failed while re-quarantining"))?;
    }

    for label in labels.iter().filter(|label| {
        (label.starts_with("quarantine-round:") && label.as_str() != round_label)
            || (label.starts_with("quarantine-until:") && label.as_str() != until_label)
            || (label.starts_with(CONTENT_HASH_LABEL_PREFIX) && label.as_str() != hash_label)
    }) {
        store
            .remove_label(&bead.id, label)
            .await
            .with_context(|| format!("remove_label({label}) failed while re-quarantining"))?;
    }

    Ok((round, until))
}

/// Park a ladder-exhausted bead behind an automatic `deferred:` window.
///
/// The park renews only while the quarantine conditions keep holding,
/// because the renewal decision is the same [`evaluate`] check that produced
/// it: the moment the content changes or the counter resets, the next expiry
/// releases the bead. Returns the deferral's expiry.
pub(crate) async fn park_expired_ladder(
    store: &dyn BeadStore,
    bead: &Bead,
) -> Result<DateTime<Utc>> {
    let until = Utc::now() + crate::deferral::window_for_round(LADDER_LAST_ROUND);
    let label = format!("deferred:{}", until.to_rfc3339());
    store
        .add_label(&bead.id, &label)
        .await
        .with_context(|| format!("add_label({label}) failed while parking an expired bead"))?;
    Ok(until)
}

/// Strip an expired quarantine marking from a bead via `store`.
///
/// The pre-ADR-022 quarantine path parked a failing bead behind a bare
/// `deferred` label written beside its `quarantine-until` window. The window
/// lapses, but nothing lifted the label — and in a workspace no worker calls
/// home, the marking alone kept the bead out of every scan, forever
/// (needle-28efee3b). This removes the bare `deferred` and every lapsed
/// `quarantine-until` window; the `failure-count` history is deliberately
/// kept, because it is the surviving record of why the bead was quarantined
/// and the evidence the next expiry check works from.
///
/// The bead's labels are re-read from the store and the marking re-checked on
/// the fresh read: a concurrent worker may have re-quarantined the bead
/// between the caller's snapshot and here, and a live hold must not be
/// lifted. Returns the labels removed; a label whose `remove_label` fails is
/// logged and skipped so a partial lift still removes what it can.
pub(crate) async fn lift_expired_marking(
    store: &dyn BeadStore,
    bead: &Bead,
) -> Result<Vec<String>> {
    let labels = store
        .labels(&bead.id)
        .await
        .context("labels() failed while lifting an expired quarantine marking")?;
    let now = Utc::now();
    if !crate::bead_store::expired_quarantine_marking(&labels, now) {
        return Ok(Vec::new());
    }

    let mut removed = Vec::new();
    for label in labels.iter().filter(|label| {
        label.trim().eq_ignore_ascii_case("deferred")
            || quarantine_until(label)
                .map(|until| until <= now)
                .unwrap_or(false)
    }) {
        match store.remove_label(&bead.id, label).await {
            Ok(()) => removed.push(label.clone()),
            Err(error) => {
                tracing::warn!(
                    bead_id = %bead.id,
                    label,
                    error = %error,
                    "failed to remove an expired quarantine marking label"
                );
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A moment `secs` from the real clock, so a window labelled "past" or
    /// "future" stays past or future however long the tests sit around. The
    /// tests only ever compare `at(x)` against `at(y)` or against `now`, and
    /// a fixed epoch base went stale the day the clock passed it.
    fn at(secs: i64) -> DateTime<Utc> {
        Utc::now() + chrono::Duration::seconds(secs)
    }

    fn quarantined_bead(labels: Vec<String>, body: &str) -> Bead {
        Bead {
            id: "q-expiry-test".to_string().into(),
            title: "Quarantine expiry test bead".to_string(),
            body: Some(body.to_string()),
            priority: 2,
            status: crate::types::BeadStatus::Open,
            assignee: None,
            labels,
            workspace: std::path::PathBuf::from("/tmp/q-expiry"),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: at(0),
            updated_at: at(0),
        }
    }

    fn full_quarantine_labels(
        round: u32,
        until: DateTime<Utc>,
        count: u32,
        body: &str,
    ) -> Vec<String> {
        let mut bead = quarantined_bead(Vec::new(), body);
        bead.labels = vec![
            "quarantined".to_string(),
            format!("quarantine-round:{round}"),
            format!("quarantine-until:{}", until.to_rfc3339()),
            format!("quarantine:failure-count:{count}"),
            format!("failure-count:{count}"),
            "verification-failed".to_string(),
            content_hash_label(&bead),
        ];
        bead.labels
    }

    #[test]
    fn no_window_label_means_no_check() {
        let bead = quarantined_bead(vec!["failure-count:9".to_string()], "body");
        assert_eq!(evaluate(&bead, at(100), 3), QuarantineExpiry::NoWindow);
    }

    #[test]
    fn future_window_is_active_regardless_of_conditions() {
        let labels = full_quarantine_labels(1, at(200), 9, "body");
        let bead = quarantined_bead(labels, "body");
        assert_eq!(evaluate(&bead, at(100), 3), QuarantineExpiry::Active);
    }

    #[test]
    fn expired_with_conditions_still_holding_requarantines() {
        let labels = full_quarantine_labels(2, at(50), 3, "body");
        let bead = quarantined_bead(labels, "body");
        assert_eq!(
            evaluate(&bead, at(100), 3),
            QuarantineExpiry::StillHolding {
                round: 2,
                failure_count: 3
            }
        );
    }

    #[test]
    fn expired_after_content_edit_is_cleared() {
        // The hash was recorded for the old body; the bead was rewritten.
        let labels = full_quarantine_labels(1, at(50), 3, "old body");
        let bead = quarantined_bead(labels, "rewritten body with a real fix");
        assert_eq!(evaluate(&bead, at(100), 3), QuarantineExpiry::Expired);
    }

    #[test]
    fn expired_after_title_edit_is_cleared() {
        let labels = full_quarantine_labels(1, at(50), 3, "body");
        let mut bead = quarantined_bead(labels, "body");
        bead.title = "Retitled: scope narrowed".to_string();
        assert_eq!(evaluate(&bead, at(100), 3), QuarantineExpiry::Expired);
    }

    #[test]
    fn legacy_quarantine_without_hash_fails_open() {
        let labels = vec![
            "quarantined".to_string(),
            "quarantine-round:1".to_string(),
            format!("quarantine-until:{}", at(50).to_rfc3339()),
            "failure-count:3".to_string(),
        ];
        let bead = quarantined_bead(labels, "body");
        assert_eq!(evaluate(&bead, at(100), 3), QuarantineExpiry::Expired);
    }

    #[test]
    fn expired_with_counter_reset_is_cleared() {
        // A success reset the counter but the window label is still there.
        let labels = vec![
            "quarantined".to_string(),
            "quarantine-round:1".to_string(),
            format!("quarantine-until:{}", at(50).to_rfc3339()),
            "failure-count:1".to_string(),
        ];
        let bead = quarantined_bead(labels, "body");
        assert_eq!(evaluate(&bead, at(100), 3), QuarantineExpiry::Expired);
    }

    #[test]
    fn expired_with_no_counter_is_cleared() {
        let labels = vec![
            "quarantined".to_string(),
            format!("quarantine-until:{}", at(50).to_rfc3339()),
        ];
        let bead = quarantined_bead(labels, "body");
        assert_eq!(evaluate(&bead, at(100), 3), QuarantineExpiry::Expired);
    }

    #[test]
    fn disabled_threshold_never_holds() {
        let labels = full_quarantine_labels(1, at(50), 9, "body");
        let bead = quarantined_bead(labels, "body");
        assert_eq!(evaluate(&bead, at(100), 0), QuarantineExpiry::Expired);
    }

    #[test]
    fn missing_round_label_starts_the_ladder_fresh() {
        let labels = vec![
            "quarantined".to_string(),
            format!("quarantine-until:{}", at(50).to_rfc3339()),
            "failure-count:3".to_string(),
            content_hash_label(&quarantined_bead(Vec::new(), "body")),
        ];
        let bead = quarantined_bead(labels, "body");
        assert_eq!(
            evaluate(&bead, at(100), 3),
            QuarantineExpiry::StillHolding {
                round: 0,
                failure_count: 3
            }
        );
    }

    #[test]
    fn content_hash_tracks_title_and_body_only() {
        let mut a = quarantined_bead(vec!["some-label".to_string()], "body");
        let mut b = quarantined_bead(vec!["other-label".to_string()], "body");
        assert_eq!(bead_content_hash(&a), bead_content_hash(&b));

        b.body = Some("edited".to_string());
        assert_ne!(bead_content_hash(&a), bead_content_hash(&b));

        b.body = Some("body".to_string());
        b.assignee = Some("worker-1".to_string());
        b.status = crate::types::BeadStatus::InProgress;
        assert_eq!(bead_content_hash(&a), bead_content_hash(&b));

        a.title = "Different title".to_string();
        assert_ne!(bead_content_hash(&a), bead_content_hash(&b));
    }

    #[test]
    fn absent_and_empty_bodies_hash_identically() {
        // `br` flips between omitting the description and emitting `""`, so
        // the two forms must not read as a content edit.
        let mut no_body = quarantined_bead(Vec::new(), "");
        no_body.body = None;
        let empty_body = quarantined_bead(Vec::new(), "");
        assert_eq!(bead_content_hash(&no_body), bead_content_hash(&empty_body));
    }

    #[test]
    fn backoff_matches_the_quarantine_ladder() {
        assert_eq!(
            capped_exponential_backoff(QUARANTINE_BASE_SECS, 0, QUARANTINE_MAX_SECS),
            2 * 60 * 60
        );
        assert_eq!(
            capped_exponential_backoff(QUARANTINE_BASE_SECS, 1, QUARANTINE_MAX_SECS),
            4 * 60 * 60
        );
        assert_eq!(
            capped_exponential_backoff(QUARANTINE_BASE_SECS, 2, QUARANTINE_MAX_SECS),
            8 * 60 * 60
        );
        // Far past the cap the window stops growing.
        assert_eq!(
            capped_exponential_backoff(QUARANTINE_BASE_SECS, 12, QUARANTINE_MAX_SECS),
            QUARANTINE_MAX_SECS
        );
    }

    /// A store that records what the lift removed, so the tests can assert on
    /// the marking actually stripped rather than on the returned list alone.
    struct LabelStore {
        labels: std::sync::Mutex<Vec<String>>,
    }

    impl LabelStore {
        fn new(labels: &[&str]) -> Self {
            LabelStore {
                labels: std::sync::Mutex::new(labels.iter().map(|l| (*l).to_string()).collect()),
            }
        }

        fn current(&self) -> Vec<String> {
            self.labels.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for LabelStore {
        fn has_valid_store(&self) -> bool {
            true
        }
        async fn ready(&self, _filters: &crate::bead_store::Filters) -> Result<Vec<Bead>> {
            anyhow::bail!("not implemented")
        }
        async fn list_all(&self) -> Result<Vec<Bead>> {
            anyhow::bail!("not implemented")
        }
        async fn show(&self, _id: &crate::types::BeadId) -> Result<Bead> {
            anyhow::bail!("not implemented")
        }
        async fn claim(
            &self,
            _id: &crate::types::BeadId,
            _actor: &str,
        ) -> Result<crate::types::ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn claim_auto(&self, _actor: &str) -> Result<crate::types::ClaimResult> {
            anyhow::bail!("not implemented")
        }
        async fn release(&self, _id: &crate::types::BeadId) -> Result<()> {
            Ok(())
        }
        async fn block(&self, _id: &crate::types::BeadId) -> Result<()> {
            Ok(())
        }
        async fn clear_assignee(&self, _id: &crate::types::BeadId) -> Result<()> {
            Ok(())
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn reopen(&self, _id: &crate::types::BeadId) -> Result<()> {
            Ok(())
        }
        async fn labels(&self, _id: &crate::types::BeadId) -> Result<Vec<String>> {
            Ok(self.current())
        }
        async fn add_label(&self, _id: &crate::types::BeadId, label: &str) -> Result<()> {
            self.labels.lock().unwrap().push(label.to_string());
            Ok(())
        }
        async fn remove_label(&self, _id: &crate::types::BeadId, label: &str) -> Result<()> {
            self.labels.lock().unwrap().retain(|l| l != label);
            Ok(())
        }
        async fn create_bead(
            &self,
            _title: &str,
            _body: &str,
            _labels: &[&str],
        ) -> Result<crate::types::BeadId> {
            anyhow::bail!("not implemented")
        }
        async fn add_dependency(
            &self,
            _blocker_id: &crate::types::BeadId,
            _blocked_id: &crate::types::BeadId,
        ) -> Result<()> {
            anyhow::bail!("not implemented")
        }
        async fn remove_dependency(
            &self,
            _blocked_id: &crate::types::BeadId,
            _blocker_id: &crate::types::BeadId,
        ) -> Result<()> {
            anyhow::bail!("not implemented")
        }
        async fn doctor_repair(&self) -> Result<crate::bead_store::RepairReport> {
            anyhow::bail!("not implemented")
        }
        async fn doctor_check(&self) -> Result<crate::bead_store::RepairReport> {
            anyhow::bail!("not implemented")
        }
        async fn full_rebuild(&self) -> Result<()> {
            anyhow::bail!("not implemented")
        }
    }

    #[tokio::test]
    async fn lift_removes_the_marker_and_the_lapsed_window_keeps_the_counter() {
        let store = LabelStore::new(&[
            "deferred",
            "failure-count:1",
            "phase-0",
            &format!("quarantine-until:{}", at(-100).to_rfc3339()),
        ]);
        let bead = quarantined_bead(store.current(), "body");

        let removed = lift_expired_marking(&store, &bead).await.unwrap();

        assert_eq!(removed.len(), 2, "{removed:?}");
        assert!(removed.contains(&"deferred".to_string()), "{removed:?}");
        let labels = store.current();
        assert!(
            !labels.iter().any(|l| l.starts_with("quarantine-until:")),
            "the lapsed window must be lifted: {labels:?}"
        );
        assert!(
            labels.contains(&"failure-count:1".to_string()),
            "the failure-count history must survive the lift: {labels:?}"
        );
        assert!(labels.contains(&"phase-0".to_string()), "{labels:?}");
    }

    #[tokio::test]
    async fn lift_leaves_an_operator_hold_and_a_live_window_alone() {
        // A bare `deferred` with no failure-count and no window is an
        // operator hold; a still-active window means the quarantine runs.
        let operator = LabelStore::new(&["deferred", "phase-0"]);
        let bead = quarantined_bead(operator.current(), "body");
        assert!(lift_expired_marking(&operator, &bead)
            .await
            .unwrap()
            .is_empty());
        assert!(operator.current().contains(&"deferred".to_string()));

        let active = LabelStore::new(&[
            "deferred",
            "failure-count:3",
            &format!("quarantine-until:{}", at(3600).to_rfc3339()),
        ]);
        let bead = quarantined_bead(active.current(), "body");
        assert!(lift_expired_marking(&active, &bead)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(active.current().len(), 3);
    }

    #[tokio::test]
    async fn lift_rechecks_fresh_labels_so_a_concurrent_requarantine_survives() {
        // The caller's snapshot says expired; the store's live labels carry a
        // newer active window (another worker re-quarantined the bead between
        // snapshot and lift). The fresh read wins: nothing is removed.
        let store = LabelStore::new(&[
            "deferred",
            "failure-count:3",
            &format!("quarantine-until:{}", at(3600).to_rfc3339()),
        ]);
        let stale_snapshot = quarantined_bead(
            vec![
                "deferred".to_string(),
                "failure-count:3".to_string(),
                format!("quarantine-until:{}", at(-100).to_rfc3339()),
            ],
            "body",
        );

        assert!(lift_expired_marking(&store, &stale_snapshot)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(store.current().len(), 3, "a live hold must not be lifted");
    }
}
