//! Detection and durable evidence for operator corrections (N-T20).
//!
//! Checkpoint JSONL and git history are compatibility inputs: the detector is
//! pure over those inputs, while the append-only writer makes replay safe.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const OPERATOR_OVERRIDE_SCHEMA_VERSION: u32 = 1;
pub const OPERATOR_OVERRIDE_LOG: &str = ".beads/operator-overrides.jsonl";
pub const REFLECTION_TRIGGER_LOG: &str = ".beads/reflection-triggers.jsonl";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverrideKind {
    Reopen,
    Revert,
    Interrupt,
}

impl OverrideKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reopen => "reopen",
            Self::Revert => "revert",
            Self::Interrupt => "interrupt",
        }
    }
}

impl std::fmt::Display for OverrideKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptOverride {
    pub schema_version: u32,
    pub attempt_id: String,
    pub bead_id: String,
    pub kind: OverrideKind,
    pub actor: String,
    pub reference: String,
    pub dedupe_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionTrigger {
    pub schema_version: u32,
    pub class: String,
    pub attempt_id: String,
    pub bead_id: String,
    pub source: String,
    pub reference: String,
    pub dedupe_key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectionReport {
    pub overrides: Vec<AttemptOverride>,
    pub reflection_triggers: Vec<ReflectionTrigger>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptOverrideAttempt {
    pub attempt_id: String,
    pub bead_id: String,
    pub commits: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct AttemptIndex {
    by_bead: BTreeMap<String, Vec<AttemptEvidence>>,
    seen_attempts: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct AttemptEvidence {
    attempt_id: String,
    bead_id: String,
    commits: Vec<String>,
}

#[derive(Debug, Clone)]
struct GitCommit {
    sha: String,
    actor: String,
    subject: String,
    body: String,
}

/// Detect operator-authored reopen and status-to-open events from checkpoint
/// forensic JSONL. Internal worker/backend recovery is excluded explicitly.
pub fn detect_forensic_overrides(
    forensic_jsonl: &str,
    registered_workers: &BTreeSet<String>,
) -> Result<Vec<AttemptOverride>> {
    let mut index = AttemptIndex::default();
    let mut events = Vec::new();
    for (line_number, line) in forensic_jsonl.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("invalid forensic JSON at line {}", line_number + 1))?;
        index_attempts(&value, &mut index);
        let event = value
            .get("event")
            .filter(|value| value.is_object())
            .cloned();
        let event = event.or_else(|| {
            (value.get("record_type").and_then(serde_json::Value::as_str) == Some("event"))
                .then_some(value)
        });
        if let Some(event) = event {
            events.push((line_number, event));
        }
    }

    let mut result = Vec::new();
    for (line_number, event) in events {
        let kind = event
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if kind != "reopened" && !is_status_to_open(&event) {
            continue;
        }
        let Some(bead_id) = event
            .get("issue_id")
            .or_else(|| event.get("bead_id"))
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let detail = event.get("detail").unwrap_or(&serde_json::Value::Null);
        let actor = event
            .get("actor")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if is_internal_reopen(actor, detail, registered_workers) {
            continue;
        }
        let Some(attempt) = index.by_bead.get(bead_id).and_then(|items| items.last()) else {
            continue;
        };
        result.push(make_override(
            OverrideKind::Reopen,
            &attempt.attempt_id,
            bead_id,
            if actor.is_empty() {
                "unknown-operator".to_string()
            } else {
                actor.to_string()
            },
            checkpoint_reference(&event, line_number),
        ));
    }
    sort_and_deduplicate(&mut result);
    Ok(result)
}

/// Detect git revert commits targeting commits attributed to an attempt.
/// `git_log` uses the NUL/record-separator format emitted by this module.
pub fn detect_git_overrides(
    git_log: &str,
    attempt_records: &[AttemptOverrideAttempt],
) -> Result<Vec<AttemptOverride>> {
    let commits = parse_git_log(git_log)?;
    let subjects = commits
        .iter()
        .map(|commit| (commit.sha.to_ascii_lowercase(), commit.subject.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut attempts_by_commit: BTreeMap<String, Vec<&AttemptOverrideAttempt>> = BTreeMap::new();
    for attempt in attempt_records {
        for commit in &attempt.commits {
            attempts_by_commit
                .entry(commit.to_ascii_lowercase())
                .or_default()
                .push(attempt);
        }
    }

    let mut result = Vec::new();
    for commit in commits {
        for target in reverted_targets(&commit, &subjects, attempt_records) {
            for (recorded_sha, attempts) in attempts_by_commit.iter().filter(|(sha, _)| {
                sha.starts_with(&target.to_ascii_lowercase())
                    || target.to_ascii_lowercase().starts_with(*sha)
            }) {
                let _ = recorded_sha;
                for attempt in attempts {
                    result.push(make_override(
                        OverrideKind::Revert,
                        &attempt.attempt_id,
                        &attempt.bead_id,
                        commit.actor.clone(),
                        format!("git:{}", commit.sha),
                    ));
                }
            }
        }
    }
    sort_and_deduplicate(&mut result);
    Ok(result)
}

/// Detect both checkpoint and git corrections without writing anything.
pub fn detect_workspace_overrides(
    workspace: &Path,
    registered_workers: &BTreeSet<String>,
) -> Result<DetectionReport> {
    let path = workspace.join(".beads/checkpoint/forensic.jsonl");
    let forensic = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut overrides = detect_forensic_overrides(&forensic, registered_workers)?;
    overrides.extend(detect_git_overrides(
        &git_log_for_repository(workspace)?,
        &parse_attempt_records(&forensic)?,
    )?);
    sort_and_deduplicate(&mut overrides);
    Ok(report_from_overrides(overrides))
}

/// Append only unseen override events and their rollback reflection triggers.
pub fn record_detections(workspace: &Path, report: &DetectionReport) -> Result<DetectionReport> {
    let beads = workspace.join(".beads");
    std::fs::create_dir_all(&beads)
        .with_context(|| format!("failed to create {}", beads.display()))?;
    let event_path = workspace.join(OPERATOR_OVERRIDE_LOG);
    let trigger_path = workspace.join(REFLECTION_TRIGGER_LOG);
    let mut seen = read_dedupe_keys(&event_path)?;
    seen.extend(read_dedupe_keys(&trigger_path)?);
    let mut events = open_append(&event_path)?;
    let mut triggers = open_append(&trigger_path)?;
    let mut new_report = DetectionReport::default();
    for override_event in &report.overrides {
        if !seen.insert(override_event.dedupe_key.clone()) {
            continue;
        }
        let trigger = report
            .reflection_triggers
            .iter()
            .find(|trigger| trigger.dedupe_key == override_event.dedupe_key)
            .cloned()
            .unwrap_or_else(|| reflection_trigger(override_event));
        serde_json::to_writer(
            &mut events,
            &serde_json::json!({"event_type":"attempt.overridden","data":override_event}),
        )?;
        serde_json::to_writer(
            &mut triggers,
            &serde_json::json!({"record_type":"reflection_trigger","trigger":trigger}),
        )?;
        use std::io::Write;
        writeln!(events)?;
        writeln!(triggers)?;
        new_report.overrides.push(override_event.clone());
        new_report.reflection_triggers.push(trigger);
    }
    Ok(new_report)
}

/// Record a live worker interrupt using the same replay-safe evidence path.
pub fn record_interrupt(
    workspace: &Path,
    attempt_id: &str,
    bead_id: &str,
    actor: &str,
    reference: &str,
) -> Result<bool> {
    let event = make_override(
        OverrideKind::Interrupt,
        attempt_id,
        bead_id,
        actor.to_string(),
        reference.to_string(),
    );
    Ok(
        !record_detections(workspace, &report_from_overrides(vec![event]))?
            .overrides
            .is_empty(),
    )
}

pub fn registered_workers_from_json(json: &str) -> Result<BTreeSet<String>> {
    let value: serde_json::Value =
        serde_json::from_str(json).context("invalid worker registry JSON")?;
    Ok(value
        .get("workers")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|worker| worker.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect())
}

fn report_from_overrides(overrides: Vec<AttemptOverride>) -> DetectionReport {
    DetectionReport {
        reflection_triggers: overrides.iter().map(reflection_trigger).collect(),
        overrides,
    }
}

fn make_override(
    kind: OverrideKind,
    attempt_id: &str,
    bead_id: &str,
    actor: String,
    reference: String,
) -> AttemptOverride {
    AttemptOverride {
        schema_version: OPERATOR_OVERRIDE_SCHEMA_VERSION,
        attempt_id: attempt_id.to_string(),
        bead_id: bead_id.to_string(),
        kind,
        actor,
        dedupe_key: dedupe_key(kind, attempt_id, &reference),
        reference,
    }
}

fn reflection_trigger(event: &AttemptOverride) -> ReflectionTrigger {
    ReflectionTrigger {
        schema_version: OPERATOR_OVERRIDE_SCHEMA_VERSION,
        class: "rollback".to_string(),
        attempt_id: event.attempt_id.clone(),
        bead_id: event.bead_id.clone(),
        source: "operator_override".to_string(),
        reference: event.reference.clone(),
        dedupe_key: event.dedupe_key.clone(),
    }
}

fn dedupe_key(kind: OverrideKind, attempt_id: &str, reference: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"needle:operator-override:v1:");
    digest.update(kind.as_str().as_bytes());
    digest.update(b":");
    digest.update(attempt_id.as_bytes());
    digest.update(b":");
    digest.update(reference.as_bytes());
    let encoded = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{encoded}")
}

fn sort_and_deduplicate(events: &mut Vec<AttemptOverride>) {
    events.sort_by(|left, right| {
        left.reference
            .cmp(&right.reference)
            .then_with(|| left.attempt_id.cmp(&right.attempt_id))
            .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
    });
    events.dedup_by(|left, right| left.dedupe_key == right.dedupe_key);
}

fn is_status_to_open(event: &serde_json::Value) -> bool {
    let kind = event
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(
        kind.as_str(),
        "updated" | "status_changed" | "status_updated" | "transitioned"
    ) {
        return false;
    }
    let detail = event.get("detail").unwrap_or(&serde_json::Value::Null);
    let prior = ["prior_base_status", "prior_status", "from", "from_status"]
        .iter()
        .find_map(|key| detail.get(*key).and_then(serde_json::Value::as_str));
    let resulting = ["resulting_base_status", "status", "to", "to_status"]
        .iter()
        .find_map(|key| detail.get(*key).and_then(serde_json::Value::as_str));
    prior.is_some_and(|value| !value.eq_ignore_ascii_case("open"))
        && resulting.is_some_and(|value| value.eq_ignore_ascii_case("open"))
}

fn is_internal_reopen(
    actor: &str,
    detail: &serde_json::Value,
    registered_workers: &BTreeSet<String>,
) -> bool {
    if registered_workers.contains(actor) {
        return true;
    }
    let detail_text = detail.to_string().to_ascii_lowercase();
    (actor.eq_ignore_ascii_case("system")
        && detail
            .get("prior_assignee")
            .and_then(serde_json::Value::as_str)
            .is_some())
        || [
            "mend",
            "mitosis",
            "false_close",
            "verification_failed",
            "verification failure",
        ]
        .iter()
        .any(|marker| detail_text.contains(marker))
}

fn checkpoint_reference(event: &serde_json::Value, line_number: usize) -> String {
    let store = event
        .get("origin_store_uuid")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown-store");
    let sequence = event
        .get("origin_event_sequence")
        .and_then(serde_json::Value::as_u64)
        .map(|value| value.to_string())
        .unwrap_or_else(|| format!("line-{}", line_number + 1));
    format!("checkpoint:{store}:{sequence}")
}

fn index_attempts(value: &serde_json::Value, index: &mut AttemptIndex) {
    let mut candidates = Vec::new();
    collect_attempt_candidates(value, &mut candidates);
    for candidate in candidates {
        if candidate.attempt_id.is_empty()
            || candidate.bead_id.is_empty()
            || !index.seen_attempts.insert(candidate.attempt_id.clone())
        {
            continue;
        }
        index
            .by_bead
            .entry(candidate.bead_id.clone())
            .or_default()
            .push(candidate);
    }
}

fn collect_attempt_candidates(value: &serde_json::Value, output: &mut Vec<AttemptEvidence>) {
    if let Some(object) = value.as_object() {
        let attempt_id = object
            .get("attempt_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let bead_id = object
            .get("bead_id")
            .or_else(|| object.get("issue_id"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !attempt_id.is_empty() && !bead_id.is_empty() {
            let mut commits = object
                .get("commits")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>();
            commits.extend(
                object
                    .get("evidence_refs")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .filter_map(|value| value.strip_prefix("commit:"))
                    .map(str::to_string),
            );
            output.push(AttemptEvidence {
                attempt_id: attempt_id.to_string(),
                bead_id: bead_id.to_string(),
                commits,
            });
        }
        for child in object.values() {
            collect_attempt_candidates(child, output);
        }
    } else if let Some(array) = value.as_array() {
        for child in array {
            collect_attempt_candidates(child, output);
        }
    }
}

fn parse_attempt_records(forensic: &str) -> Result<Vec<AttemptOverrideAttempt>> {
    let mut index = AttemptIndex::default();
    for line in forensic.lines().filter(|line| !line.trim().is_empty()) {
        let value: serde_json::Value =
            serde_json::from_str(line).context("invalid forensic JSON")?;
        index_attempts(&value, &mut index);
    }
    Ok(index
        .by_bead
        .values()
        .flatten()
        .map(|attempt| AttemptOverrideAttempt {
            attempt_id: attempt.attempt_id.clone(),
            bead_id: attempt.bead_id.clone(),
            commits: attempt.commits.clone(),
        })
        .collect())
}

fn git_log_for_repository(workspace: &Path) -> Result<String> {
    let path = workspace.to_str().context("workspace path is not UTF-8")?;
    let output = Command::new("git")
        .args([
            "-C",
            path,
            "log",
            "--no-color",
            "--format=%H%x00%an%x00%s%x00%b%x1e",
        ])
        .output()
        .with_context(|| format!("failed to run git log in {}", workspace.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "git log failed in {}: {}",
            workspace.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("git log was not UTF-8")
}

fn parse_git_log(input: &str) -> Result<Vec<GitCommit>> {
    input
        .split('\u{1e}')
        .filter(|record| !record.trim().is_empty())
        .map(|record| {
            let mut fields = record.split('\0');
            let sha = fields.next().unwrap_or_default().trim().to_string();
            let actor = fields.next().unwrap_or_default().trim().to_string();
            let subject = fields.next().unwrap_or_default().trim().to_string();
            let body = fields.next().unwrap_or_default().trim().to_string();
            if sha.is_empty() || subject.is_empty() {
                anyhow::bail!("malformed git log record");
            }
            Ok(GitCommit {
                sha,
                actor,
                subject,
                body,
            })
        })
        .collect()
}

fn reverted_targets(
    commit: &GitCommit,
    subjects: &BTreeMap<String, String>,
    attempts: &[AttemptOverrideAttempt],
) -> Vec<String> {
    let mut targets = BTreeSet::new();
    for line in commit.body.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(index) = lower.find("reverts commit ") {
            let value = line[index + "reverts commit ".len()..]
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(|character: char| !character.is_ascii_hexdigit());
            if !value.is_empty() {
                targets.insert(value.to_string());
            }
        }
        if lower.trim_start().starts_with("reverts:") {
            if let Some(value) = line.split_once(':').map(|(_, value)| value.trim()) {
                if let Some(value) = value.split_whitespace().next() {
                    if !value.is_empty() {
                        targets.insert(value.to_string());
                    }
                }
            }
        }
    }
    if targets.is_empty() && commit.subject.starts_with("Revert \"") {
        let quoted = commit
            .subject
            .strip_prefix("Revert \"")
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or_default();
        for attempt in attempts {
            for sha in &attempt.commits {
                if subjects
                    .get(&sha.to_ascii_lowercase())
                    .is_some_and(|subject| subject == quoted)
                {
                    targets.insert(sha.clone());
                }
            }
        }
    }
    targets.into_iter().collect()
}

fn open_append(path: &Path) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))
}

fn read_dedupe_keys(path: &Path) -> Result<BTreeSet<String>> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(BTreeSet::new());
    };
    let mut keys = BTreeSet::new();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        for candidate in [
            value.get("dedupe_key"),
            value.get("data").and_then(|data| data.get("dedupe_key")),
            value
                .get("trigger")
                .and_then(|trigger| trigger.get("dedupe_key")),
        ]
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        {
            keys.insert(candidate.to_string());
        }
    }
    Ok(keys)
}
