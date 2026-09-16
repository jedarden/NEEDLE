//! Escalation briefs for findings the fleet cannot fix (N-T60).
//!
//! `F5_LEARNING_LOOP_STALLED` fires precisely when the fleet has failed to
//! move the learning loop, so filing an ordinary bead would hand the work back
//! to the thing that already failed at it. The escalation is two artefacts
//! instead: a bead carrying `escalation`, which Pluck excludes by default so
//! no worker claims it, and this brief — a file a person or a stronger-model
//! lane can open and act on without first reconstructing the evidence.
//!
//! The brief is keyed by rule and scope, so a recurring stall rewrites one
//! file rather than accumulating a directory of near-identical reports. Only
//! the escalation is written here; nothing is reassigned, re-prioritised or
//! repaired, which stays true of the whole audit.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use super::factory::StalledBead;
use super::Finding;
use crate::config::Config;

/// Directory holding escalation briefs: `~/.needle/state/escalations`.
pub fn default_brief_dir(config: &Config) -> PathBuf {
    config.workspace.home.join("state").join("escalations")
}

/// A path-safe rendering of a scope.
fn slug(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The file name a finding's brief is written under: `<RULE>--<scope>.md`.
///
/// Rule and scope alone, so the same stall rewrites one file. Including
/// anything that moves — a timestamp, a count — would turn a recurring
/// escalation into a directory nobody reads.
pub fn brief_name(finding: &Finding) -> String {
    format!("{}--{}.md", slug(&finding.rule), slug(&finding.scope))
}

/// Render the brief: the evidence, the beads that are waiting, and what to do.
pub fn render_brief(finding: &Finding, stalled: &[StalledBead], at: DateTime<Utc>) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Escalation: {} in {}\n\n",
        finding.rule, finding.scope
    ));
    out.push_str(&format!(
        "Written by `needle audit` at {}. This file is rewritten in place each\n\
         time the same escalation recurs, so it always describes the current state.\n\n",
        at.to_rfc3339()
    ));

    out.push_str("## Evidence\n\n");
    out.push_str(&format!("{}\n\n", finding.detail));
    out.push_str(&format!(
        "- rule: {}\n- scope: {}\n- subject: {}\n- beads affected: {}\n\n",
        finding.rule, finding.scope, finding.subject, finding.count
    ));

    out.push_str("## Beads waiting\n\n");
    if stalled.is_empty() {
        out.push_str("None recorded.\n\n");
    } else {
        out.push_str("| bead | priority | waiting (days) | title |\n");
        out.push_str("|------|----------|----------------|-------|\n");
        for bead in stalled {
            out.push_str(&format!(
                "| {} | P{} | {} | {} |\n",
                bead.id, bead.priority, bead.age_days, bead.title
            ));
        }
        out.push('\n');
    }

    out.push_str("## Brief\n\n");
    out.push_str(
        "The fleet has not moved this work, so handing it back to the fleet is not\n\
         the answer. Run this in an interactive session or a stronger-model lane:\n\n",
    );
    out.push_str("1. Read the beads above, oldest first, and check each is still worth doing.\n");
    out.push_str(
        "2. For the first one that is, work it to a verified close in one sitting —\n   \
            a loop restarts by something closing, not by something being re-planned.\n",
    );
    out.push_str(
        "3. If a bead is not worth doing, close it with a reason. A queue that cannot\n   \
            shrink is indistinguishable from a queue nobody is working.\n",
    );
    out.push_str(
        "4. If every bead is blocked on a decision, that decision is the escalation —\n   \
            record it and say so, rather than leaving the beads open.\n\n",
    );
    out.push_str(
        "Nothing here has been reassigned or re-prioritised automatically. The audit\n\
         reports; it does not repair.\n",
    );
    out
}

/// Write the brief for `finding`, replacing any previous brief for the same
/// rule and scope.
///
/// Returns the path so the filed bead can name it: an escalation whose brief
/// an operator cannot find is the same as no brief at all.
pub fn write_brief(
    dir: &Path,
    finding: &Finding,
    stalled: &[StalledBead],
    at: DateTime<Utc>,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| {
        format!(
            "failed to create the escalation directory {}",
            dir.display()
        )
    })?;
    let path = dir.join(brief_name(finding));
    // Truncating write: the brief is rewritten per signature, never appended
    // to and never duplicated.
    std::fs::write(&path, render_brief(finding, stalled, at))
        .with_context(|| format!("failed to write the escalation brief {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-16T12:00:00Z")
            .expect("fixture stamp parses")
            .with_timezone(&Utc)
    }

    fn finding() -> Finding {
        Finding::violation(
            "F5_LEARNING_LOOP_STALLED",
            "NEEDLE",
            "learning-loop",
            "7 open unassigned learning-loop bead(s) waiting and nothing closed within 3 day(s)",
            7,
        )
    }

    fn stalled() -> Vec<StalledBead> {
        vec![
            StalledBead {
                id: "needle-old".to_string(),
                title: "Teach the loop to close".to_string(),
                priority: 1,
                age_days: 20,
            },
            StalledBead {
                id: "needle-young".to_string(),
                title: "Second loop bead".to_string(),
                priority: 0,
                age_days: 2,
            },
        ]
    }

    #[test]
    fn nt60_the_brief_names_the_evidence_the_beads_and_what_to_do() {
        let brief = render_brief(&finding(), &stalled(), now());

        assert!(brief.contains("F5_LEARNING_LOOP_STALLED"));
        assert!(brief.contains("nothing closed within 3 day(s)"), "{brief}");
        // Every waiting bead, with its priority and age.
        assert!(brief.contains("needle-old"));
        assert!(brief.contains("| P1 | 20 |"), "{brief}");
        assert!(brief.contains("needle-young"));
        assert!(brief.contains("| P0 | 2 |"), "{brief}");
        // A ready-to-run brief, not just a dump of evidence.
        assert!(brief.contains("## Brief"));
        assert!(brief.contains("verified close"));
        assert!(brief.contains("does not repair"));
    }

    #[test]
    fn nt60_the_brief_is_rewritten_per_signature_never_duplicated() {
        let dir = tempfile::TempDir::new().expect("temp dir");

        let first = write_brief(dir.path(), &finding(), &stalled(), now()).expect("brief writes");
        assert_eq!(
            first
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            Some("F5_LEARNING_LOOP_STALLED--NEEDLE.md".to_string())
        );

        // The same escalation recurring, with the numbers moved on.
        let later = Finding::violation(
            "F5_LEARNING_LOOP_STALLED",
            "NEEDLE",
            "learning-loop",
            "9 open unassigned learning-loop bead(s) waiting",
            9,
        );
        let second = write_brief(dir.path(), &later, &stalled(), now()).expect("brief rewrites");

        assert_eq!(first, second, "the same signature writes the same path");
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("dir reads")
            .filter_map(|entry| entry.ok())
            .collect();
        assert_eq!(entries.len(), 1, "one brief, rewritten — never duplicated");

        let body = std::fs::read_to_string(&second).expect("brief reads");
        assert!(body.contains("9 open unassigned"), "the rewrite is current");
        assert!(
            !body.contains("7 open unassigned"),
            "the stale evidence is gone, not appended to"
        );
    }

    /// A scope that is not path-safe must not escape the directory.
    #[test]
    fn nt60_the_brief_name_is_path_safe() {
        let nasty = Finding::violation("F5_LEARNING_LOOP_STALLED", "../../etc", "x", "d", 1);
        let name = brief_name(&nasty);
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains(".."), "{name}");
    }
}
