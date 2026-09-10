//! `needle audit` — a reachability check over state at rest.
//!
//! Three unrelated defects on 2026-09-05 starved the fleet for hours to weeks
//! each, and none was detected by anything: every health check in the system
//! runs inside a worker and asks "did I find work?", a question that cannot
//! see work the worker never looks at. This command asks the complement —
//! "is any non-closed bead unreachable, and why?" — as a reconciliation over
//! state that is already on disk.
//!
//! Nothing here samples, polls, or races. Each predicate is a pure function of
//! files at rest (bead stores, the explore config, adapter YAML, the systemd
//! unit set, the event log), so two runs over unchanged inputs give the same
//! answer, and a violation names the rule it broke rather than a symptom.
//!
//! Reference implementation: `scripts/reachability-audit.py`, validated
//! against the live estate on 2026-09-05.
//!
//! # The exit-code contract
//!
//! | code | meaning                                                        |
//! |------|----------------------------------------------------------------|
//! | 0    | clean — every registered predicate ran and found nothing       |
//! | 1    | findings — violations and/or informational states              |
//! | 2    | no verdict — inputs unreadable, a predicate errored, an empty registry, or a usage error |
//!
//! Code 2 is the one that matters. An audit that errors must never be
//! indistinguishable from an audit that passed — that is the failure mode of
//! every check that fails open. `clap` also exits 2 on a usage error, which
//! lands on the same side of the line: both mean "you did not get a verdict".
//!
//! Informational findings (rules prefixed `I_`) are states that are correct
//! as-is — usually a deliberate human gate on cross-repo work. They drive exit
//! code 1 too, because "clean" has to mean "nothing here needs a human", and a
//! gated bead does. What they must never do is reach a repair path; that rule
//! lives with the predicates that emit them.
//!
//! # The discipline predicates inherit
//!
//! The prototype's first run emitted 11 false positives from an off-by-one in
//! a ready count. A detector for silent problems is worthless the moment it is
//! noisy, because the response to noise is to stop reading it — and this audit
//! exists specifically for problems nobody is watching for. Every predicate
//! ships with the case that proves it does not fire on healthy state; the
//! fixture predicates in the tests below are the shape of that case.
//!
//! This module is the command skeleton: the finding model, the predicate
//! trait, input collection, the registry, and both output formats. The
//! predicate groups are child beads — workspace-level (needle-c1ae2730),
//! bead-level (needle-b9772de3) and flow (needle-73d53f34) — and cadence plus
//! per-finding telemetry emission belongs to needle-a0d1eb19.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::{expand_tilde_str, Config, ConfigLoader};

/// The message an empty registry fails with. Worded for an operator who has
/// just been told the estate is fine by a command that checked nothing.
const EMPTY_REGISTRY: &str =
    "no predicates registered — the audit would reconcile nothing; refusing to report a clean pass";

// ──────────────────────────────────────────────────────────────────────────────
// Finding model
// ──────────────────────────────────────────────────────────────────────────────

/// How a finding asks to be treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// A state that should not exist and needs an operator.
    Violation,
    /// A state that is correct as-is (usually a deliberate human gate),
    /// reported so the exclusion is visible. Never auto-repair.
    Informational,
}

/// One violation of one rule, at one place, with the number of beads it stands
/// for. Fleet-scale states are reported as a single finding per (rule,
/// workspace) with a count rather than one finding per bead: a detector that
/// prints 144 indistinguishable lines is a detector nobody reads to the end.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    /// Stable rule identifier, e.g. `R3_ASSIGNEE_DEAD`. Rule ids are part of
    /// the output contract — telemetry, dashboards and greps key on them, so
    /// they never get renamed, only retired.
    pub rule: String,
    pub severity: Severity,
    /// Where the finding lives. The workspace directory name for everything
    /// the current rule set produces.
    pub scope: String,
    /// The specific thing that breaks the rule: a path, a bead id, an adapter
    /// name, an assignee.
    pub subject: String,
    /// What rule was broken and why it matters — the sentence an operator
    /// reads to decide what to do.
    pub detail: String,
    /// How many beads this finding stands for. One for a single-bead finding.
    pub count: u64,
}

impl Finding {
    /// A violation: a state that should not exist.
    pub fn violation(
        rule: &str,
        scope: impl Into<String>,
        subject: impl Into<String>,
        detail: impl Into<String>,
        count: u64,
    ) -> Self {
        Finding {
            rule: rule.to_string(),
            severity: Severity::Violation,
            scope: scope.into(),
            subject: subject.into(),
            detail: detail.into(),
            count,
        }
    }

    /// An informational finding: correct as-is, never auto-repair. Rules that
    /// emit these are prefixed `I_` so the severity is legible in raw output
    /// too, not only in the parsed field.
    pub fn informational(
        rule: &str,
        scope: impl Into<String>,
        subject: impl Into<String>,
        detail: impl Into<String>,
        count: u64,
    ) -> Self {
        Finding {
            rule: rule.to_string(),
            severity: Severity::Informational,
            scope: scope.into(),
            subject: subject.into(),
            detail: detail.into(),
            count,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Inputs
// ──────────────────────────────────────────────────────────────────────────────

/// One discovered bead workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEntry {
    /// Path to the workspace root (the directory holding `.beads/`).
    pub path: PathBuf,
    /// Directory name. This is the scope findings are reported under, so it is
    /// stable across machines where the parent root moves.
    pub name: String,
}

/// The inputs every predicate reconciles, collected once before any of them
/// runs. Predicates are pure functions of this context, which is what makes
/// each of them testable against fixtures without touching the live estate:
/// build an `AuditContext` describing healthy state, assert the predicate
/// contributes nothing.
pub struct AuditContext {
    /// Root the workspace list was discovered under.
    pub root: PathBuf,
    /// Resolved global config (explore pin list, pluck exclude labels, agent
    /// adapter defaults — whatever a predicate reconciles against).
    pub config: Config,
    /// Workspaces found at depth 1 under `root`, sorted by path.
    pub workspaces: Vec<WorkspaceEntry>,
    /// Worker identities that could legitimately hold a claim right now.
    /// Empty means "no workers are live", which is a *finding-shaped* fact on
    /// a host running workers — it is never a probe failure; a probe failure
    /// fails the audit instead (see [`live_worker_identities`]).
    pub live_worker_identities: BTreeSet<String>,
}

/// Every bead workspace at depth 1 under `root`.
///
/// Depth 1 on purpose: recursive discovery finds backups, scratch clones and
/// retired trees (133 `.beads` directories against 68 real workspaces on the
/// prototype run), and a directory nobody works in is not a workspace.
///
/// The marker is a `.beads` *directory*, not `.beads/config.json` — the audit
/// must reconcile against what the Explore strand actually scans, and Explore
/// keys on the directory whatever backend the store uses. Restricting
/// discovery to bead-rs stores would leave a bf-shaped workspace with open
/// beads invisible to the reachability question, which is the exact blind
/// spot this command exists to close.
pub fn discover_workspaces(root: &Path) -> Result<Vec<WorkspaceEntry>> {
    let mut workspaces = Vec::new();
    for entry in std::fs::read_dir(root)
        .with_context(|| format!("failed to read audit root {}", root.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read an entry under {}", root.display()))?;
        let path = entry.path();
        if !path.is_dir() || !path.join(".beads").is_dir() {
            continue;
        }
        let name = match path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => continue,
        };
        workspaces.push(WorkspaceEntry { path, name });
    }
    workspaces.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(workspaces)
}

/// Worker identities that could legitimately hold a claim right now: the
/// systemd `--user` unit instances plus every `--identifier` on a live
/// command line. A claim held by an assignee outside this set is held by
/// nobody.
///
/// Fail-closed on purpose: both probes must run. "Could not ask the host" is
/// not "no workers are live" — an empty set reported as fact would turn every
/// assigned bead into a dead-assignee finding and make the one predicate the
/// fleet most needs unreadable.
pub fn live_worker_identities() -> Result<BTreeSet<String>> {
    let units = std::process::Command::new("systemctl")
        .args([
            "--user",
            "list-units",
            "--type=service",
            "--state=running",
            "needle-worker@*.service",
            "--no-legend",
            "--plain",
        ])
        .output()
        .context(
            "failed to run systemctl --user; cannot determine which worker identities are live",
        )?;
    if !units.status.success() {
        bail!(
            "systemctl --user exited with {}; cannot determine which worker identities are live",
            units.status
        );
    }

    let procs = std::process::Command::new("ps")
        .args(["-eo", "args"])
        .output()
        .context("failed to run ps; cannot determine which worker identities are live")?;
    if !procs.status.success() {
        bail!(
            "ps exited with {}; cannot determine which worker identities are live",
            procs.status
        );
    }

    let mut identities = parse_systemctl_units(&String::from_utf8_lossy(&units.stdout));
    identities.extend(parse_identifier_args(&String::from_utf8_lossy(
        &procs.stdout,
    )));
    Ok(identities)
}

/// Instance names from `systemctl --user list-units --no-legend --plain`
/// output: every whitespace token shaped `needle-worker@<id>.service`.
fn parse_systemctl_units(units_stdout: &str) -> BTreeSet<String> {
    units_stdout
        .split_whitespace()
        .filter_map(|token| token.strip_prefix("needle-worker@"))
        .map(|instance| instance.strip_suffix(".service").unwrap_or(instance))
        .filter(|instance| !instance.is_empty())
        .map(str::to_string)
        .collect()
}

/// `--identifier <id>` and `--identifier=<id>` values from `ps -eo args`
/// output. Only the long form is scanned: `-i` in some other process's
/// arguments is not evidence of a worker identity.
fn parse_identifier_args(ps_stdout: &str) -> BTreeSet<String> {
    let mut identities = BTreeSet::new();
    for line in ps_stdout.lines() {
        let mut tokens = line.split_whitespace().peekable();
        while let Some(token) = tokens.next() {
            let value = if let Some(rest) = token.strip_prefix("--identifier=") {
                Some(rest.to_string())
            } else if token == "--identifier" {
                tokens.peek().map(|value| value.to_string())
            } else {
                None
            };
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                identities.insert(value);
            }
        }
    }
    identities
}

/// Resolve the audit root and collect everything the predicates read.
///
/// `root` wins over config so an operator can scope a run (and so tests can
/// point the binary at a fixture); the default is
/// `strands.explore.workspace_root`, the same root a worker discovers under.
pub fn collect_context(config: Config, root: Option<PathBuf>) -> Result<AuditContext> {
    let root = root
        .map(|root| PathBuf::from(expand_tilde_str(&root.to_string_lossy())))
        .unwrap_or_else(|| config.strands.explore.workspace_root.clone());
    let workspaces = discover_workspaces(&root)?;
    let live_worker_identities = live_worker_identities()?;
    Ok(AuditContext {
        root,
        config,
        workspaces,
        live_worker_identities,
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// Predicates
// ──────────────────────────────────────────────────────────────────────────────

/// One predicate group: a pure function of state at rest.
///
/// Implementations read the estate through the [`AuditContext`] (and, where a
/// rule is inherently per-file, through the workspace paths it carries) and
/// return everything they find. They do not mutate anything, they do not talk
/// to workers, and an implementation that cannot answer — an unreadable store,
/// an unparseable event log — returns `Err` rather than an empty `Ok`, because
/// "this predicate saw nothing" and "this predicate could not look" must not
/// be the same result.
pub trait Predicate {
    /// The rule id findings from this predicate carry, e.g.
    /// `R1_WORKSPACE_UNSCANNED`. Informational predicates prefix theirs `I_`.
    fn id(&self) -> &'static str;

    /// One-line summary of the rule, shown where operators read the report.
    fn description(&self) -> &'static str;

    /// Reconcile the rule over the estate. Returns every violation, in any
    /// order — the report sorts them.
    fn check(&self, ctx: &AuditContext) -> Result<Vec<Finding>>;
}

/// The predicate groups registered for this build, in reporting order.
///
/// Deliberately empty until the child beads land:
///
/// - needle-c1ae2730 — `R1_WORKSPACE_UNSCANNED`, `R2_ADAPTER_MISSING`
/// - needle-b9772de3 — `R3_ASSIGNEE_DEAD` (+ the `I_HUMAN_GATED` report), `R4_LABEL_EXCLUDED`
/// - needle-73d53f34 — `R5_CLAIM_CHURN`, `R6_FRONTIER_EMPTY`
///
/// An empty registry is an error, never a clean pass. An audit that checked
/// nothing must not be able to say "no unreachable work found" — that is the
/// fail-open shape this whole design exists to refuse. Until the first group
/// lands, `needle audit` exits 2 with [`EMPTY_REGISTRY`], and the CLI test
/// pinning that tripwire is the one that breaks when the first predicate is
/// registered here.
pub fn registry() -> Vec<Box<dyn Predicate>> {
    Vec::new()
}

// ──────────────────────────────────────────────────────────────────────────────
// Running the audit
// ──────────────────────────────────────────────────────────────────────────────

/// The outcome of one audit run. Serializes without timestamps so two runs
/// over unchanged inputs are byte-identical — the determinism claim is part of
/// the contract, and it is what makes the JSON diffable in review.
#[derive(Debug, serde::Serialize)]
pub struct AuditReport {
    /// Root the run reconciled under.
    pub root: PathBuf,
    /// Workspaces discovery found. Predicates decide what "scanned" means for
    /// their own rule; the skeleton only vouches for what was discovered.
    pub workspaces_discovered: usize,
    /// Rule ids that ran, in registry order. A reader of the JSON can tell
    /// what the run actually checked — and notice when it checked nothing.
    pub predicates_run: Vec<String>,
    /// Every finding, sorted by (rule, scope, subject).
    pub findings: Vec<Finding>,
}

/// Run `predicates` over `ctx` and collect their findings.
///
/// A predicate that errors fails the whole run — there is no partial report
/// and no skipping, because a report assembled from the predicates that
/// happened to succeed is a report that looks healthier than the estate is.
pub fn run(ctx: &AuditContext, predicates: &[Box<dyn Predicate>]) -> Result<AuditReport> {
    if predicates.is_empty() {
        bail!("{EMPTY_REGISTRY}");
    }

    let mut predicates_run = Vec::with_capacity(predicates.len());
    let mut findings = Vec::new();
    for predicate in predicates {
        let found = predicate
            .check(ctx)
            .with_context(|| format!("predicate {} failed", predicate.id()))?;
        predicates_run.push(predicate.id().to_string());
        findings.extend(found);
    }

    findings.sort_by(|a, b| (&a.rule, &a.scope, &a.subject).cmp(&(&b.rule, &b.scope, &b.subject)));

    Ok(AuditReport {
        root: ctx.root.clone(),
        workspaces_discovered: ctx.workspaces.len(),
        predicates_run,
        findings,
    })
}

/// Load config, collect inputs, and run the registered predicates.
pub fn run_audit(root: Option<PathBuf>) -> Result<AuditReport> {
    let predicates = registry();
    if predicates.is_empty() {
        // Bail before touching the estate: an audit with nothing to reconcile
        // should not spend time discovering workspaces it cannot report on.
        bail!("{EMPTY_REGISTRY}");
    }
    let config = ConfigLoader::load_global()?;
    let ctx = collect_context(config, root)?;
    run(&ctx, &predicates)
}

// ──────────────────────────────────────────────────────────────────────────────
// Output contract
// ──────────────────────────────────────────────────────────────────────────────

/// The exit code the report asks for: 0 clean, 1 findings (informational
/// included), 2 being reserved for a run that never produced a report.
pub fn exit_code(report: &AuditReport) -> i32 {
    if report.findings.is_empty() {
        0
    } else {
        1
    }
}

fn beads_affected(findings: &[&Finding]) -> u64 {
    findings.iter().map(|finding| finding.count).sum()
}

/// Render the report the way an operator reads it: grouped by rule, violations
/// first, informational states clearly separated and labelled never-auto-repair.
pub fn render_human(report: &AuditReport) -> String {
    let checked = format!(
        "{} rule group(s) across {} workspace(s)",
        report.predicates_run.len(),
        report.workspaces_discovered
    );

    let violations: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Violation)
        .collect();
    let informational: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Informational)
        .collect();

    let mut out = String::new();
    if violations.is_empty() && informational.is_empty() {
        out.push_str(&format!(
            "reachability audit: no unreachable work found ({checked})\n"
        ));
        return out;
    }

    out.push_str(&format!(
        "reachability audit: {} violation(s), {} bead(s) affected ({checked})\n\n",
        violations.len(),
        beads_affected(&violations),
    ));

    // Findings are sorted by rule, so grouping is a scan for runs of equal
    // rule ids. (`[T]::chunk_by` postdates the 1.75 MSRV.)
    let mut index = 0;
    while index < violations.len() {
        let rule = violations[index].rule.as_str();
        let end = index
            + violations[index..]
                .iter()
                .take_while(|finding| finding.rule == rule)
                .count();
        let group = &violations[index..end];
        index = end;

        out.push_str(&format!("{}  ({} bead(s))\n", rule, beads_affected(group)));
        for finding in group {
            out.push_str(&format!("    {}: {}\n", finding.scope, finding.subject));
            out.push_str(&format!("        {}\n", finding.detail));
        }
        out.push('\n');
    }

    if !informational.is_empty() {
        out.push_str("informational (correct as-is, never auto-repair):\n");
        for finding in informational {
            out.push_str(&format!(
                "    {} [{}] {}: {}\n",
                finding.scope, finding.rule, finding.subject, finding.detail
            ));
        }
    }

    out
}

/// Render the report as JSON for anything that consumes it mechanically —
/// dashboards, the cadence runner, a cron that must not mistake a failure for
/// a pass. Byte-stable across runs over unchanged inputs, and it carries the
/// exit code so a consumer never has to re-derive it.
pub fn render_json(report: &AuditReport) -> Result<String> {
    let violations: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Violation)
        .collect();
    let informational: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|finding| finding.severity == Severity::Informational)
        .collect();

    let value = serde_json::json!({
        "root": report.root,
        "workspaces_discovered": report.workspaces_discovered,
        "predicates_run": report.predicates_run,
        "summary": {
            "violation_findings": violations.len(),
            "violation_beads": beads_affected(&violations),
            "informational_findings": informational.len(),
            "informational_beads": beads_affected(&informational),
        },
        "findings": report.findings,
        "exit_code": exit_code(report),
    });
    Ok(serde_json::to_string_pretty(&value)?)
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A predicate with a canned answer. The empty-findings constructor is the
    /// shape of the healthy-state case every real predicate must ship: state
    /// that is correct as-is contributes nothing, and nothing reads as clean.
    struct Fixed {
        id: &'static str,
        findings: Vec<Finding>,
    }

    impl Fixed {
        fn healthy(id: &'static str) -> Self {
            Fixed {
                id,
                findings: Vec::new(),
            }
        }

        fn reporting(id: &'static str, findings: Vec<Finding>) -> Self {
            Fixed { id, findings }
        }
    }

    impl Predicate for Fixed {
        fn id(&self) -> &'static str {
            self.id
        }

        fn description(&self) -> &'static str {
            "fixture predicate"
        }

        fn check(&self, _ctx: &AuditContext) -> Result<Vec<Finding>> {
            Ok(self.findings.clone())
        }
    }

    /// A predicate that cannot look — an unreadable store, a bad event log.
    struct Broken;

    impl Predicate for Broken {
        fn id(&self) -> &'static str {
            "R9_BROKEN"
        }

        fn description(&self) -> &'static str {
            "always errors"
        }

        fn check(&self, _ctx: &AuditContext) -> Result<Vec<Finding>> {
            bail!("state unreadable")
        }
    }

    fn test_context() -> AuditContext {
        AuditContext {
            root: PathBuf::from("/fixture-root"),
            config: Config::default(),
            workspaces: vec![WorkspaceEntry {
                path: PathBuf::from("/fixture-root/alpha"),
                name: "alpha".to_string(),
            }],
            live_worker_identities: BTreeSet::new(),
        }
    }

    fn violation(rule: &str, scope: &str, count: u64) -> Finding {
        Finding::violation(rule, scope, "subject", "detail", count)
    }

    #[test]
    fn healthy_state_is_exit_zero() {
        let ctx = test_context();
        let report = run(
            &ctx,
            &[
                Box::new(Fixed::healthy("R1_WORKSPACE_UNSCANNED")),
                Box::new(Fixed::healthy("R3_ASSIGNEE_DEAD")),
            ],
        )
        .unwrap();

        assert_eq!(exit_code(&report), 0);
        assert!(report.findings.is_empty());
        assert_eq!(
            report.predicates_run,
            vec!["R1_WORKSPACE_UNSCANNED", "R3_ASSIGNEE_DEAD"]
        );
        assert_eq!(report.workspaces_discovered, 1);
        assert!(render_human(&report).contains("no unreachable work found"));
    }

    /// The harness case every predicate inherits: over a healthy context, a
    /// predicate must contribute nothing — and the run must still vouch for
    /// what it checked, so "clean" never means "checked nothing".
    #[test]
    fn a_predicate_over_healthy_state_contributes_no_findings() {
        let ctx = test_context();
        let report = run(&ctx, &[Box::new(Fixed::healthy("R6_FRONTIER_EMPTY"))]).unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(report.predicates_run, vec!["R6_FRONTIER_EMPTY"]);
    }

    #[test]
    fn violations_exit_one_and_group_by_rule() {
        let ctx = test_context();
        let report = run(
            &ctx,
            &[Box::new(Fixed::reporting(
                "R1_WORKSPACE_UNSCANNED",
                vec![
                    violation("R1_WORKSPACE_UNSCANNED", "alpha", 954),
                    violation("R1_WORKSPACE_UNSCANNED", "beta", 1),
                ],
            ))],
        )
        .unwrap();

        assert_eq!(exit_code(&report), 1);

        let human = render_human(&report);
        assert!(human.contains("1 violation(s), 955 bead(s) affected"));
        assert!(human.contains("R1_WORKSPACE_UNSCANNED  (955 bead(s))"));
        // One line per finding scope, indented under the rule header.
        assert!(human.contains("    alpha: subject\n"));
        assert!(human.contains("    beta: subject\n"));
    }

    /// Parity with the prototype: informational-only output is still exit 1.
    /// "Clean" has to mean "nothing needs a human", and a gated bead does.
    #[test]
    fn informational_only_findings_are_exit_one_not_zero() {
        let ctx = test_context();
        let report = run(
            &ctx,
            &[Box::new(Fixed::reporting(
                "I_HUMAN_GATED",
                vec![Finding::informational(
                    "I_HUMAN_GATED",
                    "alpha",
                    "operator",
                    "deliberate gate — must NOT be auto-cleared",
                    4,
                )],
            ))],
        )
        .unwrap();

        assert_eq!(exit_code(&report), 1);

        let human = render_human(&report);
        assert!(human.contains("0 violation(s)"));
        assert!(human.contains("informational (correct as-is, never auto-repair):"));
        assert!(human.contains("alpha [I_HUMAN_GATED] operator"));
    }

    /// A predicate that cannot look fails the run. There is no partial
    /// report: findings from the predicates that did succeed must not be
    /// published beside a silent gap.
    #[test]
    fn a_predicate_failure_fails_the_whole_run() {
        let ctx = test_context();
        let error = run(
            &ctx,
            &[
                Box::new(Fixed::healthy("R1_WORKSPACE_UNSCANNED")),
                Box::new(Broken),
            ],
        )
        .unwrap_err();

        assert!(
            error.to_string().contains("R9_BROKEN"),
            "the error must name the predicate that failed: {error}"
        );
    }

    #[test]
    fn an_empty_registry_refuses_a_clean_pass() {
        let ctx = test_context();
        let error = run(&ctx, &[]).unwrap_err();
        assert!(error
            .to_string()
            .contains("refusing to report a clean pass"));
    }

    /// The determinism claim: unchanged inputs, unchanged answer, in the same
    /// order regardless of which predicate emitted a finding first.
    #[test]
    fn findings_are_sorted_deterministically() {
        let ctx = test_context();
        let reversed = vec![
            Box::new(Fixed::reporting(
                "R4_LABEL_EXCLUDED",
                vec![violation("R4_LABEL_EXCLUDED", "zeta", 1)],
            )) as Box<dyn Predicate>,
            Box::new(Fixed::reporting(
                "R1_WORKSPACE_UNSCANNED",
                vec![violation("R1_WORKSPACE_UNSCANNED", "mid", 1)],
            )),
        ];

        let first = run(&ctx, &reversed).unwrap();
        let second = run(&ctx, &reversed).unwrap();

        let rules: Vec<&str> = first.findings.iter().map(|f| f.rule.as_str()).collect();
        assert_eq!(rules, vec!["R1_WORKSPACE_UNSCANNED", "R4_LABEL_EXCLUDED"]);
        assert_eq!(render_json(&first).unwrap(), render_json(&second).unwrap());
    }

    #[test]
    fn json_output_is_stable_and_mirrors_the_exit_code() {
        let ctx = test_context();
        let report = run(
            &ctx,
            &[Box::new(Fixed::reporting(
                "R3_ASSIGNEE_DEAD",
                vec![violation("R3_ASSIGNEE_DEAD", "alpha", 144)],
            ))],
        )
        .unwrap();

        let value: serde_json::Value =
            serde_json::from_str(&render_json(&report).unwrap()).unwrap();
        assert_eq!(value["exit_code"], 1);
        assert_eq!(value["summary"]["violation_findings"], 1);
        assert_eq!(value["summary"]["violation_beads"], 144);
        assert_eq!(value["predicates_run"][0], "R3_ASSIGNEE_DEAD");
        assert_eq!(value["workspaces_discovered"], 1);
        assert_eq!(value["findings"][0]["severity"], "violation");
        assert_eq!(value["findings"][0]["count"], 144);
    }

    #[test]
    fn discovery_is_depth_one_and_sorted() {
        let root = tempfile::TempDir::new().unwrap();
        let make_workspace = |relative: &[&str]| {
            let dir = relative.iter().collect::<std::path::PathBuf>();
            std::fs::create_dir_all(root.path().join(dir).join(".beads")).unwrap();
        };
        make_workspace(&["beta"]);
        make_workspace(&["alpha"]);
        // Depth 2: a nested store — a backup or scratch clone, not a workspace.
        make_workspace(&["nested", "inner"]);
        // Not a workspace at all.
        std::fs::create_dir_all(root.path().join("plain")).unwrap();

        let found = discover_workspaces(root.path()).unwrap();
        let names: Vec<&str> = found.iter().map(|ws| ws.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn discovery_fails_closed_on_an_unreadable_root() {
        let error = discover_workspaces(Path::new("/nonexistent/audit-root")).unwrap_err();
        assert!(error.to_string().contains("failed to read audit root"));
    }

    #[test]
    fn unit_parsing_extracts_running_instances() {
        let output = "\
needle-worker@glm-needle.service loaded active running NEEDLE worker (glm-needle)
needle-worker@alpha.service    loaded active running NEEDLE worker (alpha)
plumb-clean.service            loaded active running something else\n";
        let ids = parse_systemctl_units(output);
        assert_eq!(
            ids,
            BTreeSet::from(["glm-needle".to_string(), "alpha".to_string()])
        );
    }

    #[test]
    fn identifier_parsing_handles_both_arg_forms() {
        let output = "\
/usr/bin/needle-stable run --identifier glm-needle -w /home/coding/NEEDLE
needle run --identifier=alpha
vim -i somefile
grep --identifier\n";
        let ids = parse_identifier_args(output);
        assert_eq!(
            ids,
            BTreeSet::from(["glm-needle".to_string(), "alpha".to_string()])
        );
    }
}
