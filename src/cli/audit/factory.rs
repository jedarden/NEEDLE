//! The `factory` predicate group: is the work factory itself producing?
//!
//! The reachability groups ask whether a bead can be seen. These ask whether
//! seeing it is doing any good — every rule here was a real 2026-09-15 defect
//! that a person found and no check detected:
//!
//! | rule | what went undetected |
//! |------|----------------------|
//! | `F1_WORKSPACE_NO_VERIFIED_CLOSURES` | 22 attempts in 72 h, 0 verified closures, 21 hour-long timeouts on one adapter |
//! | `F2_LATE_TIER_YIELD_INVERSION` | 86 % / 97 % "yield" on fourth and later attempts — the split-counted-as-success defect (N-T46) |
//! | `F3_UNCOSTED_TIMEOUTS` | every timed-out attempt booked at $0 (N-T47) |
//! | `F4_CI_RED` | needle-ci red for 20 h with no bead tracking it |
//! | `F6_GATE_RED_AT_HEAD` | NEEDLE's `gate_1` red at pristine HEAD 2026-10-08..16: 23 attempts on 7 beads failed it, the fix sat as a human-held proposal for 5 days |
//! | `I_CHECKLIST_DRIFT` | an open bead's checklist still unchecked after the bead it names closed |
//!
//! Each predicate is a pure function of [`AuditContext`], including its clock:
//! nothing here calls `Utc::now()`, because a predicate that reads the wall
//! clock is not reproducible and cannot be pinned by a fixture. Collection
//! happens once, in `collect_context`; a predicate only reduces what it was
//! given.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::{AuditContext, Collected, Finding, Predicate};
use crate::config::Config;
use crate::evidence_routing::LedgerRow;

/// Ledger `outcome` value for a verified closure.
pub(crate) const VERIFIED_SUCCESS: &str = "verified_success";
/// Ledger `outcome` value for an attempt nothing judged.
pub(crate) const INFRASTRUCTURE_FAILURE: &str = "infrastructure_failure";
/// Ledger `outcome` value for an attempt whose time budget expired — the
/// hour-long exit-124 timeouts F1 and F3 are counting.
pub(crate) const INDETERMINATE: &str = "indeterminate";

/// The predicates this group registers, in reporting order.
pub fn predicates() -> Vec<Box<dyn Predicate>> {
    vec![
        Box::new(WorkspaceNoVerifiedClosures),
        Box::new(LateTierYieldInversion),
        Box::new(UncostedTimeouts),
        Box::new(CiRed),
        Box::new(LearningLoopStalled),
        Box::new(GateRedAtHead),
        Box::new(ChecklistDrift),
    ]
}

// ──────────────────────────────────────────────────────────────────────────────
// Shared ledger reductions
// ──────────────────────────────────────────────────────────────────────────────

/// A ledger row's string field, or `""`.
pub(crate) fn field<'a>(row: &'a LedgerRow, key: &str) -> &'a str {
    row.data
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("")
}

/// The workspace name a ledger row belongs to.
///
/// Rows record an absolute path; findings are scoped by directory name so the
/// same workspace reads the same on two machines whose parent roots differ.
pub(crate) fn row_workspace(row: &LedgerRow) -> String {
    let raw = field(row, "workspace");
    std::path::Path::new(raw)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| raw.to_string())
}

/// Rows inside the configured window, newest bound taken from the context's
/// own collection time rather than from the clock.
///
/// An unstamped row is kept: it is still a real attempt, and dropping it would
/// quietly shrink every denominator here. (`map_or` rather than `is_none_or`,
/// which postdates the 1.75 MSRV.)
fn windowed(ctx: &AuditContext) -> Vec<&LedgerRow> {
    let cutoff = ctx.collected_at - chrono::Duration::hours(ctx.config.audit.factory.window_hours);
    ctx.ledger
        .iter()
        .filter(|row| crate::evidence_routing::is_learning_evidence_row(&row.data))
        .filter(|row| row.timestamp.is_none_or(|stamp| stamp >= cutoff))
        .collect()
}

/// Attempts an adapter or workspace was given a fair chance to win:
/// everything but infrastructure failures and decompositions (ADR-030).
pub(crate) fn judged(rows: &[&LedgerRow]) -> u64 {
    rows.iter()
        .filter(|row| {
            let outcome = field(row, "outcome");
            outcome != INFRASTRUCTURE_FAILURE && outcome != crate::attempt_accounting::DECOMPOSED
        })
        .count() as u64
}

/// Verified closures among `rows`.
pub(crate) fn verified(rows: &[&LedgerRow]) -> u64 {
    rows.iter()
        .filter(|row| field(row, "outcome") == VERIFIED_SUCCESS)
        .count() as u64
}

/// Verified closures over judged attempts, as a percentage. `None` when
/// nothing was judged — an unknown rate, never a rate of zero.
pub(crate) fn yield_points(rows: &[&LedgerRow]) -> Option<f64> {
    let judged = judged(rows);
    (judged > 0).then(|| verified(rows) as f64 * 100.0 / judged as f64)
}

/// The most common non-empty value of `key`, with its count.
fn dominant(rows: &[&LedgerRow], key: &str) -> Option<(String, u64)> {
    let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
    for row in rows {
        let value = field(row, key);
        if !value.is_empty() {
            *counts.entry(value).or_insert(0) += 1;
        }
    }
    // Highest count wins; the name breaks ties so the answer is stable.
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(value, count)| (value.to_string(), count))
}

/// Whether a row carries the schema-2 `costed` flag set true.
///
/// A free function rather than a closure: the call sites hold rows behind
/// two layers of reference, and only a real signature lets deref coercion
/// reconcile that at each of them.
pub(crate) fn costed(row: &LedgerRow) -> bool {
    row.data
        .get("costed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// Distinct non-empty values of `key`, sorted.
pub(crate) fn distinct(rows: &[&LedgerRow], key: &str) -> Vec<String> {
    rows.iter()
        .map(|row| field(row, key))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Group rows by a key function, preserving input order within each group.
///
/// The `'a` on both the input slice and the grouped values is what ties a
/// group's rows to the ledger they were borrowed from rather than to the
/// slice of references that happened to carry them in.
pub(crate) fn group_by<'a, K: Ord, F: Fn(&LedgerRow) -> K>(
    rows: &[&'a LedgerRow],
    key: F,
) -> BTreeMap<K, Vec<&'a LedgerRow>> {
    let mut groups: BTreeMap<K, Vec<&'a LedgerRow>> = BTreeMap::new();
    for row in rows {
        groups.entry(key(row)).or_default().push(*row);
    }
    groups
}

// ──────────────────────────────────────────────────────────────────────────────
// F1 — a workspace that closed nothing
// ──────────────────────────────────────────────────────────────────────────────

/// A workspace with enough judged attempts in the window to be evidence, and
/// zero verified closures among them.
struct WorkspaceNoVerifiedClosures;

impl Predicate for WorkspaceNoVerifiedClosures {
    fn id(&self) -> &'static str {
        "F1_WORKSPACE_NO_VERIFIED_CLOSURES"
    }

    fn description(&self) -> &'static str {
        "a workspace spent its attempt budget in the window and verified nothing"
    }

    fn check(&self, ctx: &AuditContext) -> Result<Vec<Finding>> {
        let factory = &ctx.config.audit.factory;
        let rows = windowed(ctx);
        let mut findings = Vec::new();

        for (workspace, rows) in group_by(&rows, row_workspace) {
            if workspace.is_empty() {
                continue;
            }
            let judged = judged(&rows);
            // The floor is what separates evidence from a small sample: a
            // workspace with three attempts and no closure is a quiet day.
            if judged < factory.min_attempts || verified(&rows) > 0 {
                continue;
            }

            let timeouts = rows
                .iter()
                .filter(|row| field(row, "outcome") == INDETERMINATE)
                .count();
            let adapters = distinct(&rows, "adapter");
            let reason = dominant(&rows, "terminal_reason")
                .map(|(reason, count)| format!("{reason} ({count} of {})", rows.len()))
                .unwrap_or_else(|| "none recorded".to_string());

            findings.push(Finding::violation(
                self.id(),
                workspace.clone(),
                workspace,
                format!(
                    "{} attempts ({} judged) in {} h closed nothing: {} timed out, \
                     adapters [{}], dominant terminal reason {}",
                    rows.len(),
                    judged,
                    factory.window_hours,
                    timeouts,
                    adapters.join(", "),
                    reason,
                ),
                judged,
            ));
        }
        Ok(findings)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// F2 — late attempts "outperforming" first attempts
// ──────────────────────────────────────────────────────────────────────────────

/// Fleet verified yield on late attempt tiers exceeding first-attempt yield.
///
/// A fourth attempt cannot really be likelier to succeed than a first: the
/// bead is by then the hardest work in the queue. An inversion means the
/// denominator is wrong somewhere, which is exactly what N-T46 found — splits
/// were being counted as successes, producing 86 % and 97 % late-tier yields.
struct LateTierYieldInversion;

impl LateTierYieldInversion {
    /// Attempt ordinals within the window, per bead, ordered in time.
    ///
    /// An unstamped row cannot be placed in the sequence, so it sorts last
    /// rather than claiming an ordinal it did not earn.
    fn tiers<'a>(rows: &[&'a LedgerRow]) -> Vec<(u32, &'a LedgerRow)> {
        let mut out = Vec::new();
        for (_, mut attempts) in group_by(rows, |row| field(row, "bead_id").to_string()) {
            attempts.sort_by_key(|row| (row.timestamp.is_none(), row.timestamp));
            for (index, row) in attempts.into_iter().enumerate() {
                out.push((index as u32 + 1, row));
            }
        }
        out
    }
}

impl Predicate for LateTierYieldInversion {
    fn id(&self) -> &'static str {
        "F2_LATE_TIER_YIELD_INVERSION"
    }

    fn description(&self) -> &'static str {
        "verified yield rises with attempt number, which means the denominator is wrong"
    }

    fn check(&self, ctx: &AuditContext) -> Result<Vec<Finding>> {
        let factory = &ctx.config.audit.factory;
        let rows = windowed(ctx);
        let tiered = Self::tiers(&rows);

        let first: Vec<&LedgerRow> = tiered
            .iter()
            .filter(|(tier, _)| *tier == 1)
            .map(|(_, row)| *row)
            .collect();
        let late: Vec<&LedgerRow> = tiered
            .iter()
            .filter(|(tier, _)| *tier >= 4)
            .map(|(_, row)| *row)
            .collect();

        // Both tiers need enough rows to be compared at all: a two-row tier
        // can hold any rate whatsoever and would flap every run.
        if (first.len() as u64) < factory.tier_min_rows
            || (late.len() as u64) < factory.tier_min_rows
        {
            return Ok(Vec::new());
        }
        let (Some(first_yield), Some(late_yield)) = (yield_points(&first), yield_points(&late))
        else {
            return Ok(Vec::new());
        };
        if late_yield - first_yield < factory.tier_inversion_points {
            return Ok(Vec::new());
        }

        Ok(vec![Finding::violation(
            self.id(),
            "fleet",
            "attempt-tier-yield",
            format!(
                "verified yield on attempt tier >= 4 is {late_yield:.0}% over {} rows \
                 against {first_yield:.0}% on tier 1 over {} rows: a later attempt cannot \
                 be likelier to succeed, so the denominator is counting something that is \
                 not a closure",
                late.len(),
                first.len(),
            ),
            late.len() as u64,
        )])
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// F3 — timeouts booked at no cost
// ──────────────────────────────────────────────────────────────────────────────

/// An adapter that reports cost for ordinary attempts but not for the ones
/// that timed out, so its spend looks smaller than it is.
struct UncostedTimeouts;

impl Predicate for UncostedTimeouts {
    fn id(&self) -> &'static str {
        "F3_UNCOSTED_TIMEOUTS"
    }

    fn description(&self) -> &'static str {
        "an adapter's timed-out attempts are booked as costing nothing"
    }

    fn check(&self, ctx: &AuditContext) -> Result<Vec<Finding>> {
        let factory = &ctx.config.audit.factory;
        let rows = windowed(ctx);
        // `costed` is the schema-2 flag (N-T47). A row without it predates the
        // flag, and a missing cost there is genuinely unknown rather than a
        // claim of zero — so those rows are not evidence either way.
        let schema_2: Vec<&LedgerRow> = rows
            .into_iter()
            .filter(|row| row.data.get("costed").is_some())
            .collect();
        let mut findings = Vec::new();

        for (adapter, rows) in group_by(&schema_2, |row| field(row, "adapter").to_string()) {
            if adapter.is_empty() {
                continue;
            }
            let costed_rows = rows.iter().filter(|row| costed(row)).count() as u64;
            // Without this floor an adapter that never reported cost at all
            // would be indistinguishable from one that stops reporting
            // exactly when an attempt times out — only the second is a defect.
            if costed_rows < factory.min_costed_rows {
                continue;
            }

            let timeouts: Vec<&&LedgerRow> = rows
                .iter()
                .filter(|row| field(row, "outcome") == INDETERMINATE)
                .collect();
            let uncosted = timeouts.iter().filter(|row| !costed(row)).count() as u64;
            if uncosted < factory.min_uncosted_timeouts || uncosted * 2 <= timeouts.len() as u64 {
                continue;
            }

            findings.push(Finding::violation(
                self.id(),
                "fleet",
                adapter.clone(),
                format!(
                    "{uncosted} of {} timed-out attempts on {adapter} carry costed=false \
                     while {costed_rows} attempts on the same adapter were costed: a killed \
                     attempt that reported no usage is unknown spend, never free",
                    timeouts.len(),
                ),
                uncosted,
            ));
        }
        Ok(findings)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// F4 — a CI gate that has been red for hours
// ──────────────────────────────────────────────────────────────────────────────

/// A workspace whose CI template has failed continuously past the threshold.
///
/// # Why an unreadable source reports nothing here
///
/// A CI source that cannot be reached contributes no finding for that
/// workspace, and the run records the unavailability as an input instead
/// (`AuditReport::inputs_unavailable`, printed in both renderers). The
/// alternative — failing the whole audit on exit code 2 — would let one
/// unreachable cluster suppress every other rule's verdict, and the audit
/// exists for the times nobody is watching.
///
/// This is not the fail-open shape the module forbids, because the report
/// never claims the CI it could not read was green: the unavailability is
/// named in the output beside the findings. What is forbidden is a silent
/// clean pass, and a named gap is not silent.
struct CiRed;

impl CiRed {
    /// The unbroken run of failures at the head of `runs`, newest first.
    ///
    /// Non-terminal runs are dropped before the walk: a queued or running
    /// workflow is not a verdict, so it neither extends a red streak nor
    /// breaks one.
    fn failing_streak(
        runs: &[crate::build_status::CiWorkflowRun],
    ) -> Vec<&crate::build_status::CiWorkflowRun> {
        runs.iter()
            .filter(|run| !run.status().is_unknown())
            .take_while(|run| run.status().is_failing())
            .collect()
    }
}

impl Predicate for CiRed {
    fn id(&self) -> &'static str {
        "F4_CI_RED"
    }

    fn description(&self) -> &'static str {
        "a workspace's CI template has been failing continuously past the threshold"
    }

    fn check(&self, ctx: &AuditContext) -> Result<Vec<Finding>> {
        let factory = &ctx.config.audit.factory;
        let mut findings = Vec::new();

        for (workspace, history) in &ctx.ci {
            let Collected::Available(runs) = history else {
                // Recorded as an unavailable input by the run; see the type
                // comment above for why this is not a false clean.
                continue;
            };
            let streak = Self::failing_streak(runs);
            let Some(oldest) = streak.last() else {
                continue;
            };
            let Some(first_failure) = oldest.created_at else {
                // An unstamped run cannot establish a duration, and "red for
                // an unknown length of time" is not the claim this rule makes.
                continue;
            };
            let red_for = ctx.collected_at - first_failure;
            if red_for < chrono::Duration::hours(factory.ci_red_hours) {
                continue;
            }

            let newest = streak[0];
            findings.push(Finding::violation(
                self.id(),
                workspace.clone(),
                ctx.ci_templates
                    .get(workspace)
                    .cloned()
                    .unwrap_or_else(|| workspace.clone()),
                format!(
                    "CI has failed every run for {:.0} h: {} consecutive failures since {}, \
                     newest {} [{}]",
                    red_for.num_minutes() as f64 / 60.0,
                    streak.len(),
                    first_failure.to_rfc3339(),
                    newest.name,
                    newest.failed_node_signature(),
                ),
                streak.len() as u64,
            ));
        }
        Ok(findings)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// F6 — a verification gate that is red at pristine HEAD
// ──────────────────────────────────────────────────────────────────────────────

/// Rule id for a gate that fails every change in a workspace.
///
/// A named constant because filing files it as a P0 operational defect
/// rather than an ordinary audit bead.
pub const F6_GATE_RED_AT_HEAD: &str = "F6_GATE_RED_AT_HEAD";

/// Semantic outcome of an attempt whose own work was judged and rejected.
const WORK_FAILURE: &str = "work_failure";

/// Attempts listed by bead and time in an F6 finding before the rest are
/// counted rather than named, so the bead body stays readable.
const GATE_RED_LISTED_ATTEMPTS: usize = 25;

/// One resolved attempt from a per-bead journal
/// (`.beads/traces/<bead>/attempts.jsonl`), reduced to what F6 reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRow {
    pub bead_id: String,
    pub recorded_at: DateTime<Utc>,
    /// Semantic outcome (`verified_success`, `work_failure`, ...).
    pub outcome: String,
    /// Terminal reason, e.g. `gate:gate_1` or `gate:gate_0,gate_1`.
    pub terminal_reason: String,
    /// The gate command named in the failure summary, when it names one.
    pub gate_command: Option<String>,
}

impl AttemptRow {
    /// The gates this attempt failed: the names in a `gate:` terminal reason
    /// on a `work_failure`, and nothing for any other attempt.
    fn failed_gates(&self) -> Vec<&str> {
        if self.outcome != WORK_FAILURE {
            return Vec::new();
        }
        self.terminal_reason
            .strip_prefix("gate:")
            .map(|names| {
                names
                    .split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The command a gate failure summary names: `command '<cmd>' failed: ...`,
/// the shape `validation` writes. `None` when the summary names none.
pub(crate) fn gate_command(summary: &str) -> Option<String> {
    let start = summary.find("command '")? + "command '".len();
    let rest = &summary[start..];
    let end = rest.find("' failed").or_else(|| rest.find('\''))?;
    let command = rest[..end].trim();
    (!command.is_empty()).then(|| command.to_string())
}

/// Attempt rows from every journal in `workspace` recorded at or after
/// `cutoff`, oldest first.
///
/// A row whose timestamp does not parse is dropped: F6 orders attempts
/// against the newest verified closure, and a row that cannot be placed in
/// time cannot be on either side of it.
pub fn attempt_rows(workspace: &Path, cutoff: DateTime<Utc>) -> Result<Vec<AttemptRow>> {
    let mut rows = Vec::new();
    for (bead, records) in crate::attempt_history::load_workspace_journals(workspace)? {
        for record in records {
            let Ok(stamp) = DateTime::parse_from_rfc3339(&record.recorded_at) else {
                continue;
            };
            let recorded_at = stamp.with_timezone(&Utc);
            if recorded_at < cutoff {
                continue;
            }
            rows.push(AttemptRow {
                bead_id: bead.clone(),
                recorded_at,
                outcome: record.outcome.clone(),
                terminal_reason: record.terminal_reason.clone().unwrap_or_default(),
                gate_command: record.failure_summary.as_deref().and_then(gate_command),
            });
        }
    }
    rows.sort_by(|a, b| (a.recorded_at, &a.bead_id).cmp(&(b.recorded_at, &b.bead_id)));
    Ok(rows)
}

/// The same gate failing `work_failure` on several distinct beads with no
/// verified closure in the workspace since: the gate is red at HEAD.
///
/// # Why the count starts at the newest verified closure
///
/// A gate that fails whatever change is put in front of it is failing on
/// what every change shares — the committed tree. One bead failing a gate is
/// a bad change; several distinct beads failing the same gate with nothing
/// passing in between is the gate. The count restarts at the workspace's
/// newest `verified_success` inside the window, so "zero verified closures"
/// holds over exactly the span the failures are counted in, and a success
/// recorded before the gate went red cannot mask it.
///
/// Retrying the beads cannot fix this, and a fix filed as a held proposal
/// sat unseen for five days, so the finding is filed as a P0 operational
/// defect (see `filing::priority_for`), never as a proposal.
struct GateRedAtHead;

impl Predicate for GateRedAtHead {
    fn id(&self) -> &'static str {
        F6_GATE_RED_AT_HEAD
    }

    fn description(&self) -> &'static str {
        "one verification gate failed several distinct beads with nothing verified since: it is red at HEAD"
    }

    fn check(&self, ctx: &AuditContext) -> Result<Vec<Finding>> {
        let config = &ctx.config.audit.gate_red;
        let cutoff = ctx.collected_at - chrono::Duration::hours(config.window_hours);
        let mut findings = Vec::new();

        for (workspace, collected) in &ctx.attempts {
            // Not collected is reported as an unavailable input by the run.
            let Collected::Available(rows) = collected else {
                continue;
            };
            let rows: Vec<&AttemptRow> = rows
                .iter()
                .filter(|row| row.recorded_at >= cutoff && row.recorded_at <= ctx.collected_at)
                .collect();
            let newest_success = rows
                .iter()
                .filter(|row| row.outcome == VERIFIED_SUCCESS)
                .map(|row| row.recorded_at)
                .max();

            // Gate name -> the failing attempts after the newest closure.
            let mut by_gate: BTreeMap<&str, Vec<&AttemptRow>> = BTreeMap::new();
            for row in rows
                .iter()
                .filter(|row| newest_success.is_none_or(|success| row.recorded_at > success))
            {
                for gate in row.failed_gates() {
                    by_gate.entry(gate).or_default().push(row);
                }
            }

            for (gate, failures) in by_gate {
                let beads: BTreeSet<&str> =
                    failures.iter().map(|row| row.bead_id.as_str()).collect();
                if (beads.len() as u64) < config.min_beads {
                    continue;
                }
                findings.push(Finding::violation(
                    self.id(),
                    workspace.clone(),
                    gate,
                    gate_red_detail(gate, &failures, &beads, newest_success, config.window_hours),
                    beads.len() as u64,
                ));
            }
        }
        Ok(findings)
    }
}

/// The detail an F6 finding carries: the gate command, the failing beads and
/// every attempt's timestamp, so the bead is actionable without the journals.
fn gate_red_detail(
    gate: &str,
    failures: &[&AttemptRow],
    beads: &BTreeSet<&str>,
    newest_success: Option<DateTime<Utc>>,
    window_hours: i64,
) -> String {
    // The newest command named wins: the gate's definition can change, and
    // the newest failure is the one that describes HEAD.
    let command = failures
        .iter()
        .rev()
        .find_map(|row| row.gate_command.as_deref())
        .map(|command| format!("`{command}`"))
        .unwrap_or_else(|| "(not named in the failure summary)".to_string());
    let since = match newest_success {
        Some(success) => format!(
            "since the newest verified closure at {}",
            success.to_rfc3339()
        ),
        None => format!("in the whole {window_hours} h window"),
    };
    let first = failures.first().map(|row| row.recorded_at.to_rfc3339());
    let last = failures.last().map(|row| row.recorded_at.to_rfc3339());
    let mut listed: Vec<String> = failures
        .iter()
        .take(GATE_RED_LISTED_ATTEMPTS)
        .map(|row| format!("{}@{}", row.bead_id, row.recorded_at.to_rfc3339()))
        .collect();
    if failures.len() > GATE_RED_LISTED_ATTEMPTS {
        listed.push(format!(
            "(+{} more)",
            failures.len() - GATE_RED_LISTED_ATTEMPTS
        ));
    }
    format!(
        "gate `{gate}` ({command}) failed work_failure on {} distinct bead(s) across {} \
         attempt(s) from {} to {}, and nothing in this workspace verified {since}. A gate \
         that fails every change is red at pristine HEAD: fix the gate at HEAD — retrying \
         the beads only burns attempts. Failing beads: {}. Attempts: {}",
        beads.len(),
        failures.len(),
        first.unwrap_or_default(),
        last.unwrap_or_default(),
        beads.iter().copied().collect::<Vec<_>>().join(", "),
        listed.join(", "),
    )
}

// ──────────────────────────────────────────────────────────────────────────────
// I_CHECKLIST_DRIFT — informational
// ──────────────────────────────────────────────────────────────────────────────

/// An open bead whose unchecked checklist line names a bead that has closed.
///
/// Informational, never repaired: the box may be unchecked because the work it
/// names was only partly delivered, and only a person can tell that from a
/// stale checkbox. Reporting it makes the drift visible without asserting
/// which way it should be resolved.
struct ChecklistDrift;

impl ChecklistDrift {
    /// Bead ids named on unchecked checklist lines of `body`.
    fn unchecked_ids(body: &str, closed: &BTreeSet<&str>) -> Vec<String> {
        let mut found = BTreeSet::new();
        for line in body.lines() {
            let trimmed = line.trim_start();
            let trimmed = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "));
            let Some(rest) = trimmed.and_then(|line| line.strip_prefix("[ ]")) else {
                continue;
            };
            // Split on everything an id cannot contain, then look the tokens
            // up. Matching on tokens rather than on substrings keeps a bead
            // id from matching inside a longer one.
            for token in rest.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_') {
                if !token.is_empty() && closed.contains(token) {
                    found.insert(token.to_string());
                }
            }
        }
        found.into_iter().collect()
    }
}

impl Predicate for ChecklistDrift {
    fn id(&self) -> &'static str {
        "I_CHECKLIST_DRIFT"
    }

    fn description(&self) -> &'static str {
        "an open bead's unchecked checklist line names a bead that is closed"
    }

    fn check(&self, ctx: &AuditContext) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();

        for workspace in &ctx.workspaces {
            let Some(Collected::Available(beads)) = ctx.beads.get(&workspace.path) else {
                continue;
            };
            let closed: BTreeSet<&str> = beads
                .iter()
                .filter(|bead| bead.status.is_done())
                .map(|bead| bead.id.as_ref())
                .collect();
            if closed.is_empty() {
                continue;
            }

            for bead in beads
                .iter()
                .filter(|b| b.status == crate::types::BeadStatus::Open)
            {
                let Some(body) = bead.body.as_deref() else {
                    continue;
                };
                let drifted = Self::unchecked_ids(body, &closed);
                if drifted.is_empty() {
                    continue;
                }
                findings.push(Finding::informational(
                    self.id(),
                    workspace.name.clone(),
                    bead.id.to_string(),
                    format!(
                        "unchecked checklist line(s) name closed bead(s): {}",
                        drifted.join(", ")
                    ),
                    drifted.len() as u64,
                ));
            }
        }
        Ok(findings)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// F5 — a learning loop that has stopped closing anything
// ──────────────────────────────────────────────────────────────────────────────

/// Label marking a bead as part of the learning loop.
pub const LOOP_LABEL: &str = "learning-loop";

/// Rule id for a stalled learning loop.
///
/// A named constant because filing keys the `escalation` label on it: this is
/// the one rule whose bead is deliberately withheld from the fleet, because
/// the fleet is precisely what has failed to move it.
pub const F5_LEARNING_LOOP_STALLED: &str = "F5_LEARNING_LOOP_STALLED";

/// One open, unassigned learning-loop bead that is waiting for someone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalledBead {
    pub id: String,
    pub title: String,
    pub priority: u8,
    pub age_days: i64,
}

/// The workspaces F5 watches: `audit.loop.workspaces`, or the home workspace
/// alone when that list is empty.
pub fn loop_workspaces_of(config: &Config) -> Vec<PathBuf> {
    if config.audit.loop_.workspaces.is_empty() {
        vec![config.audit.home_workspace.clone()]
    } else {
        config.audit.loop_.workspaces.clone()
    }
}

/// Whether a bead belongs to the learning loop.
fn is_loop_bead(bead: &crate::types::Bead) -> bool {
    bead.labels.iter().any(|label| label == LOOP_LABEL)
}

/// Open and held by nobody: the shape of a bead that is waiting rather than
/// being worked.
fn is_waiting(bead: &crate::types::Bead) -> bool {
    bead.status == crate::types::BeadStatus::Open && bead.assignee.is_none()
}

/// Open, unassigned learning-loop beads in `workspace`, longest-waiting first.
///
/// Public because the escalation brief lists exactly these, with their
/// priority and age, and recomputing them from the finding's prose would be a
/// second source of truth.
pub fn stalled_loop_beads(ctx: &AuditContext, workspace: &Path) -> Vec<StalledBead> {
    let Some(Collected::Available(beads)) = ctx.beads.get(workspace) else {
        return Vec::new();
    };
    let mut stalled: Vec<StalledBead> = beads
        .iter()
        .filter(|bead| is_loop_bead(bead) && is_waiting(bead))
        .map(|bead| StalledBead {
            id: bead.id.to_string(),
            title: bead.title.clone(),
            priority: bead.priority,
            age_days: (ctx.collected_at - bead.created_at).num_days(),
        })
        .collect();
    // Longest-waiting first: the brief is read top-down, and the bead that has
    // waited longest is the one a person should look at first. The id breaks
    // ties so two runs over unchanged inputs render byte-identically.
    stalled.sort_by(|a, b| b.age_days.cmp(&a.age_days).then_with(|| a.id.cmp(&b.id)));
    stalled
}

/// Open learning-loop beads are waiting and nothing has closed in the window.
///
/// # Why `updated_at` stands in for a close time
///
/// bead-rs exposes no close timestamp, so a closed bead's `updated_at` is the
/// best available proxy for when it closed. The approximation can only run
/// *late* — any edit after the close moves it forward, never back — so it can
/// only make this rule quieter, never noisier. A loop that genuinely stalled
/// is still reported; a loop whose last close was edited afterwards is
/// forgiven for a while longer. That asymmetry is the right one for a rule
/// whose false positive costs an operator's attention.
struct LearningLoopStalled;

impl Predicate for LearningLoopStalled {
    fn id(&self) -> &'static str {
        F5_LEARNING_LOOP_STALLED
    }

    fn description(&self) -> &'static str {
        "open learning-loop beads are waiting and no loop bead closed inside the stall window"
    }

    fn check(&self, ctx: &AuditContext) -> Result<Vec<Finding>> {
        let stall_days = ctx.config.audit.loop_.stall_days;
        let cutoff = ctx.collected_at - chrono::Duration::days(stall_days);
        let mut findings = Vec::new();

        for workspace in loop_workspaces_of(&ctx.config) {
            // Not collected means "could not look", which the run reports as
            // an unavailable input rather than as a healthy loop.
            let Some(Collected::Available(beads)) = ctx.beads.get(&workspace) else {
                continue;
            };
            let stalled = stalled_loop_beads(ctx, &workspace);
            // Nothing waiting is not a stall: a loop with no open work is a
            // loop that has nothing to do, which is a different state.
            if stalled.is_empty() {
                continue;
            }

            let newest_close = beads
                .iter()
                .filter(|bead| is_loop_bead(bead) && bead.status.is_done())
                .map(|bead| bead.updated_at)
                .max();
            if newest_close.is_some_and(|closed| closed >= cutoff) {
                continue;
            }

            let name = workspace
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| workspace.display().to_string());
            let since = match newest_close {
                Some(closed) => format!(
                    "newest loop close {} ({} day(s) ago, approximated by updated_at)",
                    closed.to_rfc3339(),
                    (ctx.collected_at - closed).num_days()
                ),
                None => "no learning-loop bead has ever closed here".to_string(),
            };
            let oldest = stalled
                .first()
                .map(|bead| {
                    format!(
                        "oldest {} (P{}, waiting {} day(s))",
                        bead.id, bead.priority, bead.age_days
                    )
                })
                .unwrap_or_default();

            findings.push(Finding::violation(
                self.id(),
                name,
                LOOP_LABEL,
                format!(
                    "{} open unassigned learning-loop bead(s) waiting and nothing closed \
                     within {stall_days} day(s): {since}; {oldest}",
                    stalled.len(),
                ),
                stalled.len() as u64,
            ));
        }
        Ok(findings)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_status::{CiWorkflowRun, FailedNode};
    use crate::cli::audit::{exit_code, run, WorkspaceEntry};
    use crate::types::Bead;
    use chrono::{DateTime, Utc};
    use std::path::PathBuf;

    /// Fixed collection time every fixture is written against, so no test
    /// depends on when it runs.
    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-15T22:00:00Z")
            .expect("fixture stamp parses")
            .with_timezone(&Utc)
    }

    fn ctx() -> AuditContext {
        AuditContext {
            root: PathBuf::from("/fixture-root"),
            config: crate::config::Config::default(),
            workspaces: Vec::new(),
            live_worker_identities: BTreeSet::new(),
            collected_at: now(),
            ledger: Vec::new(),
            ci: BTreeMap::new(),
            ci_templates: BTreeMap::new(),
            beads: BTreeMap::new(),
            attempts: BTreeMap::new(),
            tree_drift: BTreeMap::new(),
        }
    }

    /// One ledger row `hours_ago` before the fixture's collection time.
    fn row(hours_ago: i64, workspace: &str, bead: &str, adapter: &str, outcome: &str) -> LedgerRow {
        LedgerRow {
            timestamp: Some(now() - chrono::Duration::hours(hours_ago)),
            data: serde_json::json!({
                "workspace": workspace,
                "bead_id": bead,
                "adapter": adapter,
                "provisional": false,
                "outcome": outcome,
            }),
        }
    }

    fn with_field(mut row: LedgerRow, key: &str, value: serde_json::Value) -> LedgerRow {
        row.data[key] = value;
        row
    }

    fn bead(id: &str, status: crate::types::BeadStatus, labels: &[&str]) -> Bead {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "title": id,
            "description": null,
            "priority": 1,
            "status": status,
            "assignee": "",
            "labels": labels,
            "source_repo": "/fixture-root/NEEDLE",
            "created_at": (now() - chrono::Duration::days(10)).to_rfc3339(),
            "updated_at": now().to_rfc3339(),
        }))
        .expect("fixture bead deserializes")
    }

    fn findings_of(predicate: &dyn Predicate, ctx: &AuditContext) -> Vec<Finding> {
        predicate.check(ctx).expect("predicate runs")
    }

    // ── F6 ────────────────────────────────────────────────────────────────

    /// The NEEDLE attempt history the October 2026 red gate left behind,
    /// as (recorded_at, bead, outcome, terminal_reason): `gate_1` (`cargo
    /// test --lib`) red at pristine HEAD, every attempt failing it whatever
    /// its change. The two `needle-test` closures are real rows too — a test
    /// fixture's, recorded into the live journals — and they are why F6
    /// counts from the newest closure rather than demanding an empty window.
    const OCTOBER_2026_GATE_1: &[(&str, &str, &str, &str)] = &[
        (
            "2026-10-08T18:05:43.836Z",
            "needle-closed",
            "verified_success",
            "",
        ),
        (
            "2026-10-09T19:49:32.292Z",
            "needle-f54bab82",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-11T15:46:39.224Z",
            "needle-d7b6198a",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-11T16:03:57.065Z",
            "needle-d74c803a",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-11T16:15:08.960Z",
            "needle-d7b6198a",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-11T16:35:03.325Z",
            "needle-d74c803a",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-11T16:53:06.598Z",
            "needle-d7b6198a",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-11T16:58:49.363Z",
            "needle-test",
            "verified_success",
            "",
        ),
        (
            "2026-10-11T16:58:49.378Z",
            "needle-test",
            "verified_success",
            "",
        ),
        (
            "2026-10-11T17:10:08.033Z",
            "needle-d74c803a",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-11T17:59:30.830Z",
            "needle-78a9a735",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-12T04:45:40.486Z",
            "needle-d74c803a",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-12T05:12:01.709Z",
            "needle-d74c803a",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-12T05:22:05.743Z",
            "needle-69306987",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-12T05:46:08.009Z",
            "needle-b7ca91bf",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-12T06:21:06.366Z",
            "needle-69306987",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-12T07:34:24.431Z",
            "needle-b184d5d7",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-12T08:05:27.847Z",
            "needle-b184d5d7",
            "work_failure",
            "gate:gate_1",
        ),
        (
            "2026-10-12T08:37:14.400Z",
            "needle-b184d5d7",
            "indeterminate",
            "timeout:idle",
        ),
        (
            "2026-10-14T00:21:32.505Z",
            "needle-d7b6198a",
            "work_failure",
            "gate:gate_1",
        ),
    ];

    /// Write `rows` as real `attempts.jsonl` journals under `workspace`, in
    /// the on-disk record shape the outcome handler appends.
    fn write_journals(workspace: &std::path::Path, rows: &[(&str, &str, &str, &str)]) {
        let mut by_bead: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for (index, (at, bead, outcome, reason)) in rows.iter().enumerate() {
            let summary = reason.strip_prefix("gate:").map(|gate| {
                format!(
                    "gate `{gate}` failed:\ncommand 'cargo test --lib' failed: Compiling needle \
                     v0.6.36\nerror: test failed, to rerun pass `--lib`"
                )
            });
            let record = serde_json::json!({
                "schema_version": 1,
                "attempt_id": format!("attempt-{index}"),
                "recorded_at": at,
                "worker": "codex-gpt-5.6-luna-xhigh-sg-codex-1",
                "adapter": "codex-gpt-5.6-luna-xhigh",
                "outcome": outcome,
                "terminal_reason": (!reason.is_empty()).then_some(*reason),
                "exit_code": 0,
                "requested_action": "released:gate_failed",
                "commits": [],
                "duration_ms": 600000,
                "failure_summary": summary,
            });
            by_bead.entry(bead).or_default().push(record.to_string());
        }
        for (bead, lines) in by_bead {
            let path = crate::attempt_history::history_path(
                workspace,
                &crate::types::BeadId::from(bead.to_string()),
            );
            std::fs::create_dir_all(path.parent().expect("journal has a parent"))
                .expect("journal directory");
            std::fs::write(&path, lines.join("\n") + "\n").expect("journal written");
        }
    }

    /// A context collected at `at` over the journals under `workspace`,
    /// through the same reader and window trim production uses.
    fn gate_ctx(workspace: &std::path::Path, at: DateTime<Utc>) -> AuditContext {
        let mut context = ctx();
        context.collected_at = at;
        let cutoff = at - chrono::Duration::hours(context.config.audit.gate_red.window_hours);
        context.attempts.insert(
            "NEEDLE".to_string(),
            Collected::Available(attempt_rows(workspace, cutoff).expect("journals read")),
        );
        context
    }

    fn at(stamp: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(stamp)
            .expect("fixture stamp parses")
            .with_timezone(&Utc)
    }

    /// needle-9c66c160 acceptance (5): replayed over the real October
    /// history, F6 fires on every daily audit from 2026-10-11 to 10-14 and
    /// goes quiet once only one bead is failing inside the window.
    #[test]
    fn f6_fires_on_the_october_2026_needle_gate_1_history() {
        let workspace = tempfile::tempdir().expect("tempdir");
        write_journals(workspace.path(), OCTOBER_2026_GATE_1);

        // The daily audit runs at 07:40 America/New_York, ~11:45 UTC.
        for day in ["2026-10-11", "2026-10-12", "2026-10-13", "2026-10-14"] {
            let context = gate_ctx(workspace.path(), at(&format!("{day}T11:45:00Z")));
            let findings = findings_of(&GateRedAtHead, &context);
            assert_eq!(findings.len(), 1, "{day}: {findings:?}");
            let finding = &findings[0];
            assert_eq!(finding.rule, F6_GATE_RED_AT_HEAD);
            assert_eq!(finding.scope, "NEEDLE");
            assert_eq!(finding.subject, "gate_1");
            assert!(finding.count >= 2, "{day}: {finding:?}");
            assert!(
                finding.detail.contains("`cargo test --lib`"),
                "the gate command is named: {}",
                finding.detail
            );
        }

        // On 10-13 the window reaches back past the needle-test closures to
        // nothing; every bead failing since then is named, with timestamps.
        let context = gate_ctx(workspace.path(), at("2026-10-13T11:45:00Z"));
        let finding = &findings_of(&GateRedAtHead, &context)[0];
        assert_eq!(finding.count, 5, "{}", finding.detail);
        for bead in [
            "needle-d74c803a",
            "needle-78a9a735",
            "needle-69306987",
            "needle-b7ca91bf",
            "needle-b184d5d7",
        ] {
            assert!(finding.detail.contains(bead), "{bead}: {}", finding.detail);
        }
        assert!(finding
            .detail
            .contains("needle-78a9a735@2026-10-11T17:59:30.830+00:00"));
        assert!(finding.detail.contains("2026-10-11T16:58:49.378+00:00"));

        // 10-15: one failing bead in the window is one bad change, not a gate.
        let context = gate_ctx(workspace.path(), at("2026-10-15T11:45:00Z"));
        assert!(findings_of(&GateRedAtHead, &context).is_empty());
    }

    #[test]
    fn f6_stays_quiet_when_one_bead_fails_a_gate_repeatedly() {
        let workspace = tempfile::tempdir().expect("tempdir");
        write_journals(
            workspace.path(),
            &[
                (
                    "2026-09-15T10:00:00Z",
                    "needle-a",
                    "work_failure",
                    "gate:gate_1",
                ),
                (
                    "2026-09-15T11:00:00Z",
                    "needle-a",
                    "work_failure",
                    "gate:gate_1",
                ),
                (
                    "2026-09-15T12:00:00Z",
                    "needle-a",
                    "work_failure",
                    "gate:gate_1",
                ),
            ],
        );
        assert!(findings_of(&GateRedAtHead, &gate_ctx(workspace.path(), now())).is_empty());
    }

    #[test]
    fn f6_resets_at_a_verified_closure_and_never_combines_gates() {
        let workspace = tempfile::tempdir().expect("tempdir");
        write_journals(
            workspace.path(),
            &[
                // Two beads failed gate_1, then something verified: the gate
                // was passable after all.
                (
                    "2026-09-15T10:00:00Z",
                    "needle-a",
                    "work_failure",
                    "gate:gate_1",
                ),
                (
                    "2026-09-15T11:00:00Z",
                    "needle-b",
                    "work_failure",
                    "gate:gate_1",
                ),
                ("2026-09-15T12:00:00Z", "needle-c", "verified_success", ""),
                // After it, two beads failed two different gates.
                (
                    "2026-09-15T13:00:00Z",
                    "needle-d",
                    "work_failure",
                    "gate:gate_0",
                ),
                (
                    "2026-09-15T14:00:00Z",
                    "needle-e",
                    "work_failure",
                    "gate:gate_1",
                ),
                // Infrastructure and timeouts are not gate verdicts.
                (
                    "2026-09-15T15:00:00Z",
                    "needle-f",
                    "infrastructure_failure",
                    "gate_error:gate_1",
                ),
                (
                    "2026-09-15T16:00:00Z",
                    "needle-g",
                    "indeterminate",
                    "timeout",
                ),
            ],
        );
        assert!(findings_of(&GateRedAtHead, &gate_ctx(workspace.path(), now())).is_empty());

        // A multi-gate terminal reason counts toward each gate it names.
        write_journals(
            workspace.path(),
            &[(
                "2026-09-15T17:00:00Z",
                "needle-h",
                "work_failure",
                "gate:gate_0,gate_1",
            )],
        );
        let findings = findings_of(&GateRedAtHead, &gate_ctx(workspace.path(), now()));
        let subjects: Vec<&str> = findings.iter().map(|f| f.subject.as_str()).collect();
        assert_eq!(subjects, vec!["gate_0", "gate_1"], "{findings:?}");
    }

    #[test]
    fn f6_reads_the_gate_command_from_the_failure_summary() {
        assert_eq!(
            gate_command("gate `gate_1` failed:\ncommand 'cargo test --lib' failed: boom"),
            Some("cargo test --lib".to_string())
        );
        assert_eq!(
            gate_command("command 'scripts/definition-of-done.sh --fast' failed: x"),
            Some("scripts/definition-of-done.sh --fast".to_string())
        );
        assert_eq!(gate_command("exit code 1"), None);
        assert_eq!(gate_command("command '' failed: x"), None);
    }

    // ── F5 ────────────────────────────────────────────────────────────────

    /// A learning-loop bead, optionally closed `closed_days_ago`.
    fn loop_bead(id: &str, status: crate::types::BeadStatus, closed_days_ago: Option<i64>) -> Bead {
        let mut bead = bead(id, status, &[LOOP_LABEL]);
        if let Some(days) = closed_days_ago {
            bead.updated_at = now() - chrono::Duration::days(days);
        }
        bead
    }

    /// The loop workspace a fixture watches, wired through config so the
    /// predicate reads it the way production does.
    fn loop_ctx(beads: Vec<Bead>) -> AuditContext {
        let mut context = ctx();
        let workspace = PathBuf::from("/fixture-root/NEEDLE");
        context.config.audit.loop_.workspaces = vec![workspace.clone()];
        context.workspaces.push(WorkspaceEntry {
            path: workspace.clone(),
            name: "NEEDLE".to_string(),
        });
        context.beads.insert(workspace, Collected::Available(beads));
        context
    }

    #[test]
    fn nt60_f5_reports_a_loop_whose_newest_close_is_four_days_old() {
        let context = loop_ctx(vec![
            bead(
                "needle-open-1",
                crate::types::BeadStatus::Open,
                &[LOOP_LABEL],
            ),
            bead(
                "needle-open-2",
                crate::types::BeadStatus::Open,
                &[LOOP_LABEL],
            ),
            loop_bead("needle-closed", crate::types::BeadStatus::Closed, Some(4)),
        ]);

        let findings = findings_of(&LearningLoopStalled, &context);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].scope, "NEEDLE");
        assert_eq!(findings[0].subject, LOOP_LABEL);
        assert_eq!(findings[0].count, 2, "both waiting beads are counted");
        assert!(
            findings[0].detail.contains("approximated by updated_at"),
            "the detail names the approximation: {}",
            findings[0].detail
        );
    }

    #[test]
    fn nt60_f5_stays_quiet_when_a_loop_bead_closed_inside_the_window() {
        // A close one day ago: the loop is alive.
        let context = loop_ctx(vec![
            bead("needle-open", crate::types::BeadStatus::Open, &[LOOP_LABEL]),
            loop_bead("needle-closed", crate::types::BeadStatus::Closed, Some(1)),
        ]);
        assert!(findings_of(&LearningLoopStalled, &context).is_empty());
    }

    #[test]
    fn nt60_f5_stays_quiet_without_an_open_unassigned_loop_bead() {
        // Nothing waiting: a loop with no open work is not a stalled loop.
        let context = loop_ctx(vec![loop_bead(
            "needle-closed",
            crate::types::BeadStatus::Closed,
            Some(30),
        )]);
        assert!(findings_of(&LearningLoopStalled, &context).is_empty());

        // Open but already claimed: somebody is on it.
        let mut assigned = bead(
            "needle-claimed",
            crate::types::BeadStatus::Open,
            &[LOOP_LABEL],
        );
        assigned.assignee = Some("glm-needle".to_string());
        let context = loop_ctx(vec![assigned]);
        assert!(findings_of(&LearningLoopStalled, &context).is_empty());
    }

    #[test]
    fn nt60_the_loop_watches_the_home_workspace_by_default() {
        let mut config = crate::config::Config::default();
        config.audit.home_workspace = PathBuf::from("/fixture-root/NEEDLE");
        config.audit.loop_.workspaces.clear();
        assert_eq!(
            loop_workspaces_of(&config),
            vec![PathBuf::from("/fixture-root/NEEDLE")],
            "an empty list means the home workspace alone"
        );

        config.audit.loop_.workspaces = vec![PathBuf::from("/srv/other")];
        assert_eq!(
            loop_workspaces_of(&config),
            vec![PathBuf::from("/srv/other")],
            "a configured list replaces the default"
        );
    }

    #[test]
    fn nt60_stalled_beads_are_longest_waiting_first_with_priority_and_age() {
        let mut young = bead(
            "needle-young",
            crate::types::BeadStatus::Open,
            &[LOOP_LABEL],
        );
        young.created_at = now() - chrono::Duration::days(2);
        let mut old = bead("needle-old", crate::types::BeadStatus::Open, &[LOOP_LABEL]);
        old.created_at = now() - chrono::Duration::days(20);

        let context = loop_ctx(vec![young, old]);
        let stalled = stalled_loop_beads(&context, Path::new("/fixture-root/NEEDLE"));

        let ids: Vec<&str> = stalled.iter().map(|bead| bead.id.as_str()).collect();
        assert_eq!(ids, vec!["needle-old", "needle-young"]);
        assert_eq!(stalled[0].age_days, 20);
        assert_eq!(stalled[0].priority, 1);
    }

    // ── F1 ────────────────────────────────────────────────────────────────

    #[test]
    fn nt57_f1_reports_a_workspace_that_verified_nothing() {
        let mut context = ctx();
        // The NEEDLE workspace as it stood on 2026-09-15: 22 attempts, none
        // verified, 21 of them hour-long timeouts on one adapter.
        for index in 0..21 {
            context.ledger.push(row(
                index % 70,
                "/home/coding/NEEDLE",
                &format!("needle-{index}"),
                "claude-code-glm-5.3-flash",
                INDETERMINATE,
            ));
        }
        context.ledger.push(row(
            5,
            "/home/coding/NEEDLE",
            "needle-22",
            "claude-code-glm-5.3-flash",
            "work_failure",
        ));
        // A healthy workspace in the same ledger must contribute nothing.
        for index in 0..12 {
            context.ledger.push(row(
                index,
                "/home/coding/SEAM",
                &format!("seam-{index}"),
                "codex",
                if index % 2 == 0 {
                    VERIFIED_SUCCESS
                } else {
                    "work_failure"
                },
            ));
        }

        let findings = findings_of(&WorkspaceNoVerifiedClosures, &context);
        assert_eq!(findings.len(), 1, "only the stalled workspace reports");
        assert_eq!(findings[0].scope, "NEEDLE");
        assert_eq!(findings[0].count, 22, "all 22 attempts were judged");
        assert!(
            findings[0].detail.contains("21 timed out"),
            "detail names the timeouts: {}",
            findings[0].detail
        );
        assert!(findings[0].detail.contains("claude-code-glm-5.3-flash"));
    }

    #[test]
    fn nt16_factory_findings_ignore_provisional_rows() {
        let mut context = ctx();
        for index in 0..22 {
            context.ledger.push(with_field(
                row(
                    1,
                    "/home/coding/NEEDLE",
                    &format!("needle-provisional-{index}"),
                    "claude-code-glm-5.3-flash",
                    INDETERMINATE,
                ),
                "provisional",
                serde_json::json!(true),
            ));
        }

        assert!(findings_of(&WorkspaceNoVerifiedClosures, &context).is_empty());
    }

    #[test]
    fn nt57_f1_stays_quiet_below_the_evidence_floor_and_on_any_closure() {
        let mut context = ctx();
        // Nine attempts, none verified: under the floor, so a quiet day.
        for index in 0..9 {
            context.ledger.push(row(
                1,
                "/srv/thin",
                &format!("thin-{index}"),
                "codex",
                "work_failure",
            ));
        }
        assert!(findings_of(&WorkspaceNoVerifiedClosures, &context).is_empty());

        // Over the floor but with one closure: producing, so not a finding.
        let mut context = ctx();
        for index in 0..14 {
            context.ledger.push(row(
                1,
                "/srv/busy",
                &format!("busy-{index}"),
                "codex",
                if index == 0 {
                    VERIFIED_SUCCESS
                } else {
                    "work_failure"
                },
            ));
        }
        assert!(findings_of(&WorkspaceNoVerifiedClosures, &context).is_empty());

        // Rows older than the window are not this window's evidence.
        let mut context = ctx();
        for index in 0..20 {
            context.ledger.push(row(
                200,
                "/srv/stale",
                &format!("stale-{index}"),
                "codex",
                "work_failure",
            ));
        }
        assert!(findings_of(&WorkspaceNoVerifiedClosures, &context).is_empty());
    }

    // ── F2 ────────────────────────────────────────────────────────────────

    #[test]
    fn nt57_f2_reports_late_tier_yield_above_first_attempt_yield() {
        let mut context = ctx();
        // 25 beads: attempt 1 fails, attempts 2-5 "succeed". The fourth and
        // fifth attempts then out-yield the first by 100 points, which is the
        // shape the split-counted-as-success defect produced.
        for bead_index in 0..25 {
            let bead = format!("b-{bead_index}");
            for attempt in 0..5 {
                let outcome = if attempt == 0 {
                    "work_failure"
                } else {
                    VERIFIED_SUCCESS
                };
                context.ledger.push(row(
                    (50 - attempt) as i64,
                    "/srv/w",
                    &bead,
                    "codex",
                    outcome,
                ));
            }
        }

        let findings = findings_of(&LateTierYieldInversion, &context);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].scope, "fleet");
        assert_eq!(findings[0].subject, "attempt-tier-yield");
        assert!(
            findings[0].detail.contains("100%") && findings[0].detail.contains("0%"),
            "detail carries both yields: {}",
            findings[0].detail
        );
    }

    #[test]
    fn nt57_f2_stays_quiet_without_an_inversion_or_without_enough_rows() {
        // Healthy: later attempts do worse, as they must.
        let mut context = ctx();
        for bead_index in 0..25 {
            let bead = format!("b-{bead_index}");
            for attempt in 0..5 {
                let outcome = if attempt == 0 {
                    VERIFIED_SUCCESS
                } else {
                    "work_failure"
                };
                context.ledger.push(row(
                    (50 - attempt) as i64,
                    "/srv/w",
                    &bead,
                    "codex",
                    outcome,
                ));
            }
        }
        assert!(findings_of(&LateTierYieldInversion, &context).is_empty());

        // A real inversion, but on too few rows to be more than noise.
        let mut context = ctx();
        for bead_index in 0..3 {
            let bead = format!("b-{bead_index}");
            for attempt in 0..5 {
                let outcome = if attempt == 0 {
                    "work_failure"
                } else {
                    VERIFIED_SUCCESS
                };
                context.ledger.push(row(
                    (50 - attempt) as i64,
                    "/srv/w",
                    &bead,
                    "codex",
                    outcome,
                ));
            }
        }
        assert!(findings_of(&LateTierYieldInversion, &context).is_empty());
    }

    // ── F3 ────────────────────────────────────────────────────────────────

    #[test]
    fn nt57_f3_reports_an_adapter_whose_timeouts_are_booked_free() {
        let mut context = ctx();
        // Eight ordinary attempts carry a cost; six of seven timeouts do not.
        for index in 0..8 {
            context.ledger.push(with_field(
                row(
                    2,
                    "/srv/w",
                    &format!("c-{index}"),
                    "glm-flash",
                    "work_failure",
                ),
                "costed",
                serde_json::json!(true),
            ));
        }
        for index in 0..6 {
            context.ledger.push(with_field(
                row(
                    2,
                    "/srv/w",
                    &format!("t-{index}"),
                    "glm-flash",
                    INDETERMINATE,
                ),
                "costed",
                serde_json::json!(false),
            ));
        }
        context.ledger.push(with_field(
            row(2, "/srv/w", "t-6", "glm-flash", INDETERMINATE),
            "costed",
            serde_json::json!(true),
        ));

        let findings = findings_of(&UncostedTimeouts, &context);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].subject, "glm-flash");
        assert_eq!(findings[0].count, 6);
        assert!(findings[0].detail.contains("6 of 7 timed-out attempts"));
    }

    #[test]
    fn nt57_f3_ignores_adapters_that_never_costed_and_pre_schema_2_rows() {
        // An adapter that reports no cost at all is a capability gap, not the
        // selective under-reporting this rule exists to catch.
        let mut context = ctx();
        for index in 0..9 {
            context.ledger.push(with_field(
                row(
                    2,
                    "/srv/w",
                    &format!("t-{index}"),
                    "never-costs",
                    INDETERMINATE,
                ),
                "costed",
                serde_json::json!(false),
            ));
        }
        assert!(findings_of(&UncostedTimeouts, &context).is_empty());

        // Rows predating the flag carry no claim about cost either way.
        let mut context = ctx();
        for index in 0..9 {
            context.ledger.push(row(
                2,
                "/srv/w",
                &format!("t-{index}"),
                "old",
                INDETERMINATE,
            ));
        }
        for index in 0..9 {
            context.ledger.push(row(
                2,
                "/srv/w",
                &format!("c-{index}"),
                "old",
                "work_failure",
            ));
        }
        assert!(findings_of(&UncostedTimeouts, &context).is_empty());
    }

    // ── F4 ────────────────────────────────────────────────────────────────

    fn ci_run(name: &str, hours_ago: i64, phase: &str) -> CiWorkflowRun {
        CiWorkflowRun {
            name: name.to_string(),
            phase: phase.to_string(),
            created_at: Some(now() - chrono::Duration::hours(hours_ago)),
            message: None,
            failed_nodes: vec![FailedNode {
                display_name: "verify-fast".to_string(),
                message: "main: Error (exit code 128)".to_string(),
            }],
            commit_sha: None,
            source: Some("test".to_string()),
        }
    }

    #[test]
    fn nt57_f4_reports_a_template_red_for_twenty_hours() {
        let mut context = ctx();
        context
            .ci_templates
            .insert("NEEDLE".to_string(), "needle-ci".to_string());
        context.ci.insert(
            "NEEDLE".to_string(),
            Collected::Available(vec![
                ci_run("needle-ci-newest", 1, "Failed"),
                // A run still going is not a verdict and must not break the streak.
                ci_run("needle-ci-running", 2, "Running"),
                ci_run("needle-ci-mid", 9, "Error"),
                ci_run("needle-ci-first", 20, "Failed"),
                ci_run("needle-ci-green", 26, "Succeeded"),
            ]),
        );

        let findings = findings_of(&CiRed, &context);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].scope, "NEEDLE");
        assert_eq!(findings[0].subject, "needle-ci");
        assert_eq!(
            findings[0].count, 3,
            "three completed failures, the pending run excluded"
        );
        assert!(
            findings[0].detail.contains("20 h"),
            "{}",
            findings[0].detail
        );
        assert!(
            findings[0]
                .detail
                .contains("verify-fast: main: Error (exit code 128)"),
            "detail carries the failed-node signature: {}",
            findings[0].detail
        );
    }

    #[test]
    fn nt57_f4_stays_quiet_inside_the_threshold_and_when_ci_cannot_be_read() {
        // A single fresh failure is not an outage: CI is allowed to be red
        // for the minutes between a bad push and its fix.
        let mut context = ctx();
        context.ci.insert(
            "NEEDLE".to_string(),
            Collected::Available(vec![
                ci_run("needle-ci-new", 0, "Failed"),
                ci_run("needle-ci-green", 4, "Succeeded"),
            ]),
        );
        assert!(findings_of(&CiRed, &context).is_empty());

        // Green head: nothing to report at all.
        let mut context = ctx();
        context.ci.insert(
            "NEEDLE".to_string(),
            Collected::Available(vec![ci_run("needle-ci-green", 1, "Succeeded")]),
        );
        assert!(findings_of(&CiRed, &context).is_empty());

        // Unreadable source: no finding, and the run surfaces the gap as an
        // unavailable input rather than as a clean CI.
        let mut context = ctx();
        context.ci.insert(
            "NEEDLE".to_string(),
            Collected::Unavailable("CI unavailable: connection refused".to_string()),
        );
        assert!(findings_of(&CiRed, &context).is_empty());
        let report = run(&context, &predicates()).expect("audit runs");
        assert!(
            report
                .inputs_unavailable
                .iter()
                .any(|line| line.contains("CI unavailable")),
            "the gap is named in the report: {:?}",
            report.inputs_unavailable
        );
    }

    // ── I_CHECKLIST_DRIFT ─────────────────────────────────────────────────

    #[test]
    fn nt57_checklist_drift_is_informational_and_names_the_closed_bead() {
        let mut context = ctx();
        let path = PathBuf::from("/fixture-root/NEEDLE");
        context.workspaces.push(WorkspaceEntry {
            path: path.clone(),
            name: "NEEDLE".to_string(),
        });

        let mut parent = bead("needle-parent", crate::types::BeadStatus::Open, &[]);
        parent.body = Some(
            "## Progress\n\
             - [x] needle-done1 landed\n\
             - [ ] needle-done2 still open in this checklist\n\
             - [ ] needle-open is genuinely open\n"
                .to_string(),
        );
        context.beads.insert(
            path,
            Collected::Available(vec![
                parent,
                bead("needle-done1", crate::types::BeadStatus::Closed, &[]),
                bead("needle-done2", crate::types::BeadStatus::Closed, &[]),
                bead("needle-open", crate::types::BeadStatus::Open, &[]),
            ]),
        );

        let findings = findings_of(&ChecklistDrift, &context);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].severity,
            crate::cli::audit::Severity::Informational
        );
        assert_eq!(findings[0].subject, "needle-parent");
        assert!(
            findings[0].detail.contains("needle-done2"),
            "names the closed bead: {}",
            findings[0].detail
        );
        assert!(
            !findings[0].detail.contains("needle-done1"),
            "a checked line is not drift: {}",
            findings[0].detail
        );
        assert!(
            !findings[0].detail.contains("needle-open"),
            "an open bead on an unchecked line is not drift: {}",
            findings[0].detail
        );
    }

    // ── The group as a whole ──────────────────────────────────────────────

    #[test]
    fn nt57_the_factory_group_is_registered_and_healthy_state_is_clean() {
        let ids: Vec<&str> = predicates().iter().map(|p| p.id()).collect();
        assert_eq!(
            ids,
            vec![
                "F1_WORKSPACE_NO_VERIFIED_CLOSURES",
                "F2_LATE_TIER_YIELD_INVERSION",
                "F3_UNCOSTED_TIMEOUTS",
                "F4_CI_RED",
                "F5_LEARNING_LOOP_STALLED",
                "F6_GATE_RED_AT_HEAD",
                "I_CHECKLIST_DRIFT",
            ]
        );
        let mut registered = ids.clone();
        registered.push(crate::cli::audit::tree_drift::F7_STALE_TREE_DRIFT);
        assert_eq!(
            crate::cli::audit::registry()
                .iter()
                .map(|p| p.id())
                .collect::<Vec<_>>(),
            registered,
            "the registry is the factory group plus the checkout-drift rule"
        );

        // The healthy-state case every predicate inherits: an estate with
        // nothing wrong contributes nothing, and the run still vouches for
        // what it checked.
        let report = run(&ctx(), &predicates()).expect("audit runs");
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(exit_code(&report), 0);
        assert_eq!(report.predicates_run.len(), 7);
    }
}
