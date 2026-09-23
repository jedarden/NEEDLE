//! Regression coverage for durable notes on deduplicated native bead-rs alerts.

use anyhow::{Context, Result};
use chrono::DateTime;

use needle::bead_store::BeadStore;
use needle::fingerprint::{
    append_alert_note, build_alert_labels, check_alert_deduplication, AlertDeduplication, AlertKind,
};

#[tokio::test]
async fn deduplicated_alert_note_is_persisted() -> Result<()> {
    let workspace = super::create_test_workspace("alert-note-persistence")?;
    let store = super::store_for_workspace(workspace.path())?;
    let workspace_name = workspace.path().display().to_string();
    let kind = AlertKind::PulseFinding;
    let cause = "repeated scanner finding";
    let fingerprint = needle::fingerprint::compute_fingerprint(&workspace_name, &kind, cause);
    let labels = build_alert_labels(&fingerprint, &["pulse-finding"]);
    let label_refs: Vec<&str> = labels.iter().map(String::as_str).collect();

    let alert_id = store
        .create_bead("Repeated scanner finding", cause, &label_refs)
        .await?;

    let deduplication = check_alert_deduplication(&store, &workspace_name, &kind, cause).await?;
    let deduplicated_id = match deduplication {
        AlertDeduplication::Deduplicated { bead_id, .. } => bead_id,
        other => anyhow::bail!("expected an open alert to deduplicate, got {other:?}"),
    };
    assert_eq!(deduplicated_id, alert_id);

    append_alert_note(&store, &deduplicated_id, "repeat observed").await?;

    let notes = store
        .notes(&deduplicated_id)
        .await?
        .context("native bead-rs did not return the persisted alert note")?;
    let occurrence = notes
        .lines()
        .find(|line| line.ends_with("] repeat observed"))
        .context("persisted notes did not contain the repeated alert occurrence")?;
    let (timestamp, message) = occurrence
        .split_once("] ")
        .context("persisted alert note did not have the expected timestamp prefix")?;
    DateTime::parse_from_rfc3339(timestamp.trim_start_matches('['))
        .context("persisted alert note timestamp was not RFC 3339")?;
    assert_eq!(message, "repeat observed");

    Ok(())
}
