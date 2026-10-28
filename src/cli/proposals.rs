//! Operator review of held, near-duplicate proposal backlogs.
//!
//! This command deliberately has no worker or scheduler entry point. Similarity
//! only proposes a review group; a named group is required for every close.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::Builder as TempFileBuilder;

use crate::cli::audit::discover_workspaces;
use crate::config::{expand_tilde_str, resolve_bead_cli, BeadBackend, CliOverrides, ConfigLoader};
use crate::strand::weave::candidate_filter::{jaccard, similarity_tokens, EVIDENCE_HEADING};
use crate::telemetry::Telemetry;

#[derive(Debug, Subcommand)]
pub enum ProposalsCommand {
    /// Find possible duplicate held proposals, or apply named operator-approved groups.
    Dedupe {
        /// Root containing workspace directories (defaults to the Explore scan root).
        #[arg(long)]
        root: Option<PathBuf>,
        /// Print a machine-readable report.
        #[arg(long)]
        json: bool,
        /// Close non-survivors in the explicitly named groups.
        #[arg(long, requires = "group")]
        apply: bool,
        /// Stable group ID to apply; repeat for multiple approved groups.
        #[arg(long, requires = "apply")]
        group: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedWorkspace {
    pub workspace: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    High,
    Low,
}

impl Tier {
    fn name(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Low => "low",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupMember {
    pub id: String,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub revision: u64,
    pub similarity_to_survivor: f64,
    pub shared_artifacts: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub id: String,
    pub workspace: PathBuf,
    pub tier: Tier,
    pub survivor: String,
    pub members: Vec<GroupMember>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DedupeReport {
    pub root: PathBuf,
    pub workspaces_covered: Vec<PathBuf>,
    pub skipped: Vec<SkippedWorkspace>,
    pub high_groups: Vec<Group>,
    pub low_groups: Vec<Group>,
}

impl DedupeReport {
    fn all_groups(&self) -> impl Iterator<Item = &Group> {
        self.high_groups.iter().chain(self.low_groups.iter())
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RawBead {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    status: String,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    notes: Option<String>,
    created_at: DateTime<Utc>,
    revision: u64,
}

impl RawBead {
    fn eligible(&self) -> bool {
        self.status == "open"
            && self.assignee.as_deref().is_none_or(str::is_empty)
            && self.labels.iter().any(|label| label == "proposal-pending")
    }
}

struct Proposal {
    bead: RawBead,
    tokens: BTreeSet<String>,
    artifacts: BTreeSet<String>,
    evidence_items: usize,
}

impl Proposal {
    fn new(bead: RawBead, path_pattern: &Regex, workspace: &Path) -> Self {
        let body = bead.description.as_deref().unwrap_or("");
        Self {
            tokens: similarity_tokens(&bead.title, body),
            artifacts: artifacts(&bead.title, body, path_pattern, workspace),
            evidence_items: evidence_item_count(body, workspace),
            bead,
        }
    }
}

/// Entry point for `needle proposals`.
pub fn run(command: ProposalsCommand) -> Result<()> {
    match command {
        ProposalsCommand::Dedupe {
            root,
            json,
            apply,
            group,
        } => {
            let config = ConfigLoader::load_global()?;
            let root = root.unwrap_or_else(|| config.strands.explore.workspace_root.clone());
            let root = PathBuf::from(expand_tilde_str(&root.to_string_lossy()))
                .canonicalize()
                .with_context(|| format!("cannot resolve proposal root {}", root.display()))?;
            let report = collect_report(&root)?;
            if !apply {
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print_report(&report);
                }
                emit_telemetry(
                    &config.telemetry,
                    vec![(
                        "proposals.dedupe.report",
                        json!({
                            "root": root,
                            "workspaces_covered": report.workspaces_covered.len(),
                            "workspaces_skipped": report.skipped.len(),
                            "high_groups": report.high_groups.len(),
                            "low_groups": report.low_groups.len(),
                        }),
                    )],
                )?;
                return Ok(());
            }

            let outcomes = apply_groups(&report, &group);
            if json {
                println!("{}", serde_json::to_string_pretty(&outcomes)?);
            } else {
                for outcome in &outcomes {
                    println!(
                        "{}: {} ({})",
                        outcome.group, outcome.outcome, outcome.detail
                    );
                }
            }
            let events = outcomes
                .iter()
                .map(|outcome| {
                    (
                        "proposals.dedupe.group",
                        json!({
                            "group": outcome.group,
                            "outcome": outcome.outcome,
                            "detail": outcome.detail,
                        }),
                    )
                })
                .collect();
            emit_telemetry(&config.telemetry, events)?;
            if outcomes.iter().any(|outcome| outcome.outcome != "applied") {
                bail!("one or more requested proposal groups were refused");
            }
            Ok(())
        }
    }
}

/// Recompute all review groups from the current bead stores. This reads only.
pub fn collect_report(root: &Path) -> Result<DedupeReport> {
    let mut report = DedupeReport {
        root: root.to_path_buf(),
        workspaces_covered: Vec::new(),
        skipped: Vec::new(),
        high_groups: Vec::new(),
        low_groups: Vec::new(),
    };
    let path_pattern = Regex::new(r"[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*\.[A-Za-z][A-Za-z0-9_]*")?;
    for workspace in discover_workspaces(root)? {
        let marker = workspace.path.join(".beads/config.json");
        if !marker.is_file() {
            report.skipped.push(SkippedWorkspace {
                workspace: workspace.path,
                reason: "not a bead-rs workspace (.beads/config.json absent)".to_string(),
            });
            continue;
        }
        let (config, _) = match ConfigLoader::load_resolved(
            &workspace.path,
            CliOverrides {
                workspace: Some(workspace.path.clone()),
                ..Default::default()
            },
        ) {
            Ok(config) => config,
            Err(error) => {
                report.skipped.push(SkippedWorkspace {
                    workspace: workspace.path,
                    reason: format!("configuration unavailable: {error:#}"),
                });
                continue;
            }
        };
        if config.bead_cli.backend == BeadBackend::Br {
            report.skipped.push(SkippedWorkspace {
                workspace: workspace.path,
                reason: "workspace is bound to the legacy backend".to_string(),
            });
            continue;
        }
        let (_, binary, _) = resolve_bead_cli(&config.bead_cli)
            .with_context(|| format!("cannot locate bead-rs for {}", workspace.path.display()))?;
        let raw = bead_command(
            &binary,
            &workspace.path,
            &["list", "--status", "open", "--json", "--limit", "999999"],
        )?;
        let beads = parse_bead_list(&raw)
            .with_context(|| format!("cannot parse bead list for {}", workspace.path.display()))?;
        let proposals: Vec<Proposal> = beads
            .into_iter()
            .filter(RawBead::eligible)
            .map(|bead| Proposal::new(bead, &path_pattern, &workspace.path))
            .collect();
        let threshold = config.strands.weave.duplicate_similarity_threshold;
        let (high, low) = group_workspace(&workspace.path, proposals, threshold);
        report.high_groups.extend(high);
        report.low_groups.extend(low);
        report.workspaces_covered.push(workspace.path);
    }
    Ok(report)
}

fn group_workspace(
    workspace: &Path,
    proposals: Vec<Proposal>,
    threshold: f64,
) -> (Vec<Group>, Vec<Group>) {
    let mut order: Vec<usize> = (0..proposals.len()).collect();
    order.sort_by(|&a, &b| {
        proposals[b]
            .evidence_items
            .cmp(&proposals[a].evidence_items)
            .then_with(|| {
                proposals[a]
                    .bead
                    .created_at
                    .cmp(&proposals[b].bead.created_at)
            })
            .then_with(|| proposals[a].bead.id.cmp(&proposals[b].bead.id))
    });
    // A proposal may be a high-confidence match to one survivor and a low-
    // confidence match to another. Keep the tier partitions independent so
    // the low-tier review does not silently lose that second signal.
    let high = groups_for_tier(
        workspace,
        &proposals,
        &order,
        threshold,
        Tier::High,
        &mut BTreeSet::new(),
    );
    let low = groups_for_tier(
        workspace,
        &proposals,
        &order,
        threshold,
        Tier::Low,
        &mut BTreeSet::new(),
    );
    (high, low)
}

fn groups_for_tier(
    workspace: &Path,
    proposals: &[Proposal],
    order: &[usize],
    threshold: f64,
    tier: Tier,
    used: &mut BTreeSet<usize>,
) -> Vec<Group> {
    let mut groups = Vec::new();
    for &survivor in order {
        if used.contains(&survivor) {
            continue;
        }
        let mut members = Vec::new();
        for &other in order {
            if other == survivor || used.contains(&other) {
                continue;
            }
            let similarity = jaccard(&proposals[survivor].tokens, &proposals[other].tokens);
            if similarity < threshold {
                continue;
            }
            let shared = shared_artifacts(&proposals[survivor], &proposals[other]);
            if (tier == Tier::High) == shared.is_empty() {
                continue;
            }
            members.push((other, similarity, shared));
        }
        if members.is_empty() {
            continue;
        }
        let mut survivor_shared = BTreeSet::new();
        for (_, _, shared) in &members {
            survivor_shared.extend(shared.iter().cloned());
        }
        let mut rows = vec![member_row(
            &proposals[survivor],
            1.0,
            survivor_shared.into_iter().collect(),
        )];
        for (index, similarity, shared) in &members {
            rows.push(member_row(&proposals[*index], *similarity, shared.clone()));
        }
        rows[1..].sort_by(|a, b| a.id.cmp(&b.id));
        let id = group_id(workspace, &rows);
        groups.push(Group {
            id,
            workspace: workspace.to_path_buf(),
            tier,
            survivor: proposals[survivor].bead.id.clone(),
            members: rows,
        });
        used.insert(survivor);
        for (index, _, _) in members {
            used.insert(index);
        }
    }
    groups
}

fn member_row(proposal: &Proposal, similarity: f64, shared: Vec<String>) -> GroupMember {
    GroupMember {
        id: proposal.bead.id.clone(),
        title: proposal.bead.title.clone(),
        created_at: proposal.bead.created_at,
        revision: proposal.bead.revision,
        similarity_to_survivor: similarity,
        shared_artifacts: shared,
    }
}

fn shared_artifacts(a: &Proposal, b: &Proposal) -> Vec<String> {
    a.artifacts.intersection(&b.artifacts).cloned().collect()
}

fn group_id(workspace: &Path, members: &[GroupMember]) -> String {
    let mut ids: Vec<&str> = members.iter().map(|member| member.id.as_str()).collect();
    ids.sort_unstable();
    let mut digest = Sha256::new();
    digest.update(workspace.to_string_lossy().as_bytes());
    digest.update([0]);
    for id in ids {
        digest.update(id.as_bytes());
        digest.update([0]);
    }
    format!("dedupe-{:x}", digest.finalize())[..23].to_string()
}

fn evidence_item_count(body: &str, workspace: &Path) -> usize {
    let Some(section) = body.split_once(EVIDENCE_HEADING).map(|(_, rest)| rest) else {
        return 0;
    };
    section
        .lines()
        .take_while(|line| !line.trim_start().starts_with("## ") && !line.starts_with("---"))
        .filter(|line| {
            let item = line.trim_start().strip_prefix("- ").unwrap_or("");
            if let Some(rest) = item.strip_prefix("missing artifact: `") {
                let Some((path, _)) = rest.split_once('`') else {
                    return false;
                };
                return safe_relative(path) && !workspace.join(path).exists();
            }
            if item.starts_with("failing command: `") {
                return item.contains("` (observed: ") && !item.ends_with("(observed: )");
            }
            let Some(rest) = item.strip_prefix('`') else {
                return false;
            };
            let Some((location, quoted)) = rest.split_once('`') else {
                return false;
            };
            let Some((path, line)) = location.rsplit_once(':') else {
                return false;
            };
            let Some(line) = line.parse::<usize>().ok().filter(|line| *line > 0) else {
                return false;
            };
            let Some(quote) = quoted.split('`').nth(1) else {
                return false;
            };
            if !safe_relative(path) || quote.chars().count() < 5 {
                return false;
            }
            let file = workspace.join(path);
            if file
                .metadata()
                .map_or(true, |meta| !meta.is_file() || meta.len() > 4 * 1024 * 1024)
            {
                return false;
            }
            let Ok(text) = std::fs::read_to_string(file) else {
                return false;
            };
            let lines: Vec<&str> = text.lines().collect();
            if line > lines.len() {
                return false;
            }
            let first = line.saturating_sub(3);
            let last = (line + 2).min(lines.len());
            let window = lines[first..last]
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            window.contains(&quote.split_whitespace().collect::<Vec<_>>().join(" "))
        })
        .count()
}

fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

fn artifacts(title: &str, body: &str, pattern: &Regex, workspace: &Path) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    let evidence = body
        .split_once(EVIDENCE_HEADING)
        .map(|(_, rest)| rest)
        .unwrap_or("");
    for text in [title, body] {
        for found in pattern.find_iter(text) {
            let path = found.as_str();
            if !safe_relative(path) {
                continue;
            }
            let prefix = &text[..found.start()];
            if prefix.ends_with("//") || prefix.ends_with('@') {
                continue;
            }
            // A bare dotted word is a file citation only when that file
            // exists. A missing path must be explicitly cited as evidence.
            if !workspace.join(path).is_file()
                && !evidence
                    .lines()
                    .any(|line| line.trim_start().starts_with("- ") && line.contains(path))
            {
                continue;
            }
            paths.insert(path.to_string());
        }
    }
    // An exact shared Evidence at HEAD bullet is a cited artifact even when
    // it describes a failing command rather than a file path.
    for line in evidence.lines().take_while(|line| {
        let trimmed = line.trim_start();
        !trimmed.starts_with("## ") && !trimmed.starts_with("---")
    }) {
        if let Some(item) = line.trim_start().strip_prefix("- ") {
            let item = item.trim();
            if !item.is_empty() {
                paths.insert(format!("Evidence at HEAD: {item}"));
            }
        }
    }
    paths
}

// bead-rs 0.2.6 prints one object per line for `list --json` (and `[]`
// when empty); 0.3.0 prints one array. Both are active in fleet workspaces.
fn parse_bead_list(raw: &[u8]) -> Result<Vec<RawBead>> {
    let output = std::str::from_utf8(raw).context("bead list was not UTF-8")?;
    if output.trim_start().starts_with('[') {
        return serde_json::from_str(output).context("invalid bead list array");
    }
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("invalid bead list JSONL row"))
        .collect()
}

// bead-rs 0.2.6 wraps `show --json` in a one-element array; 0.3.0
// returns the object directly.
fn parse_bead_show(raw: &[u8]) -> Result<RawBead> {
    let output = std::str::from_utf8(raw).context("bead show was not UTF-8")?;
    if output.trim_start().starts_with('[') {
        let mut rows: Vec<RawBead> =
            serde_json::from_str(output).context("invalid bead show array")?;
        if rows.len() != 1 {
            bail!("bead show returned {} rows, expected one", rows.len());
        }
        return Ok(rows.remove(0));
    }
    serde_json::from_str(output).context("invalid bead show object")
}

fn bead_command(binary: &Path, workspace: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new(binary)
        .current_dir(workspace)
        .args(args)
        .output()
        .with_context(|| format!("cannot run bead in {}", workspace.display()))?;
    if !output.status.success() {
        bail!(
            "bead {} failed in {} ({}): {}",
            args.first().copied().unwrap_or("command"),
            workspace.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

#[derive(Debug, Serialize)]
struct ApplyOutcome {
    group: String,
    outcome: &'static str,
    detail: String,
}

fn apply_groups(report: &DedupeReport, ids: &[String]) -> Vec<ApplyOutcome> {
    let mut outcomes = Vec::new();
    let known: BTreeMap<&str, &Group> = report
        .all_groups()
        .map(|group| (group.id.as_str(), group))
        .collect();
    let mut seen = BTreeSet::new();
    for id in ids {
        if !seen.insert(id.as_str()) {
            outcomes.push(ApplyOutcome {
                group: id.clone(),
                outcome: "refused",
                detail: "group ID repeated".to_string(),
            });
            continue;
        }
        let Some(group) = known.get(id.as_str()) else {
            outcomes.push(ApplyOutcome {
                group: id.clone(),
                outcome: "refused",
                detail: "group absent from the recomputed report or member set changed".to_string(),
            });
            continue;
        };
        match apply_group(group) {
            Ok(closed) => outcomes.push(ApplyOutcome {
                group: id.clone(),
                outcome: "applied",
                detail: format!("closed {} duplicate(s)", closed),
            }),
            Err(error) => outcomes.push(ApplyOutcome {
                group: id.clone(),
                outcome: "refused",
                detail: format!("{error:#}"),
            }),
        }
    }
    outcomes
}

fn apply_group(group: &Group) -> Result<usize> {
    let (config, _) = ConfigLoader::load_resolved(
        &group.workspace,
        CliOverrides {
            workspace: Some(group.workspace.clone()),
            ..Default::default()
        },
    )?;
    if config.bead_cli.backend == BeadBackend::Br
        || !group.workspace.join(".beads/config.json").is_file()
    {
        bail!("workspace is no longer bead-rs");
    }
    let (_, binary, _) = resolve_bead_cli(&config.bead_cli)?;

    // Validate the entire group before the first write. The manifest also
    // guards every member's revision inside one transaction.
    let mut survivor_notes = None;
    for member in &group.members {
        let raw = bead_command(&binary, &group.workspace, &["show", &member.id, "--json"])?;
        let current = parse_bead_show(&raw)
            .with_context(|| format!("cannot parse current bead {}", member.id))?;
        if current.revision != member.revision {
            bail!(
                "{} changed revision from {} to {}",
                member.id,
                member.revision,
                current.revision
            );
        }
        if current.status != "open" {
            bail!("{} is no longer open", member.id);
        }
        if current
            .assignee
            .as_deref()
            .is_some_and(|assignee| !assignee.is_empty())
        {
            bail!("{} gained an assignee", member.id);
        }
        if !current
            .labels
            .iter()
            .any(|label| label == "proposal-pending")
        {
            bail!("{} lost proposal-pending", member.id);
        }
        if member.id == group.survivor {
            survivor_notes = Some(current.notes.unwrap_or_default());
        }
    }

    let mut closed = Vec::new();
    let mut operations = Vec::new();
    for member in group
        .members
        .iter()
        .filter(|member| member.id != group.survivor)
    {
        let reason = format!(
            "Duplicate of {} (needle proposals dedupe group {}, operator-approved)",
            group.survivor, group.id
        );
        operations.push(json!({
            "op": "close",
            "id": member.id,
            "reason": reason,
            "if_revision": member.revision,
        }));
        operations.push(json!({
            "op": "dep_add",
            "blocked": member.id,
            "blocker": group.survivor,
            "kind": "relates_to",
        }));
        closed.push(member.id.clone());
    }

    let note = format!(
        "Operator-approved proposal dedupe group {}: closed duplicates {}.",
        group.id,
        closed.join(", ")
    );
    let mut notes = survivor_notes.context("survivor absent from group")?;
    if !notes.is_empty() {
        notes.push('\n');
    }
    notes.push_str(&note);
    let survivor_revision = group
        .members
        .iter()
        .find(|member| member.id == group.survivor)
        .context("survivor absent from group")?
        .revision;
    operations.push(json!({
        "op": "update",
        "id": group.survivor,
        "notes": notes,
        "if_revision": survivor_revision,
    }));

    // bead-rs 0.2.6 has no comment command. A manifest uses the same guarded
    // close and dependency primitives while committing the entire group or
    // nothing, including the survivor note. Keep note content out of argv.
    let mut manifest = TempFileBuilder::new()
        .prefix("proposal-dedupe-")
        .tempfile_in(group.workspace.join(".beads"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        manifest
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    serde_json::to_writer(
        &mut manifest,
        &json!({"manifest_version": 1, "operations": operations}),
    )?;
    manifest.flush()?;
    let path = manifest
        .path()
        .to_str()
        .context("manifest path is not UTF-8")?;
    bead_command(
        &binary,
        &group.workspace,
        &["manifest", "commit", "--input", path],
    )
    .with_context(|| format!("atomic apply of group {} failed", group.id))?;
    Ok(closed.len())
}

fn print_report(report: &DedupeReport) {
    println!(
        "Proposal dedupe: {} workspace(s) covered, {} skipped; {} high and {} low group(s)",
        report.workspaces_covered.len(),
        report.skipped.len(),
        report.high_groups.len(),
        report.low_groups.len()
    );
    for skipped in &report.skipped {
        println!(
            "Skipped {}: {}",
            skipped.workspace.display(),
            skipped.reason
        );
    }
    for (tier, groups) in [
        (Tier::High, &report.high_groups),
        (Tier::Low, &report.low_groups),
    ] {
        println!("\n{} tier:", tier.name());
        for group in groups {
            println!(
                "  {} [{}] survivor {}",
                group.id,
                group.workspace.display(),
                group.survivor
            );
            for member in &group.members {
                println!(
                    "    {} r{} {:.3}: {} ({}) [{}]",
                    member.id,
                    member.revision,
                    member.similarity_to_survivor,
                    member.title,
                    member.created_at,
                    member.shared_artifacts.join(", ")
                );
            }
        }
    }
}

fn emit_telemetry(
    config: &crate::config::TelemetryConfig,
    events: Vec<(&str, Value)>,
) -> Result<()> {
    let telemetry =
        Telemetry::from_config("needle-proposals".to_string(), config).unwrap_or_else(|error| {
            tracing::warn!(error = %error, "failed to configure proposal telemetry");
            Telemetry::new("needle-proposals".to_string())
        });
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        telemetry.start_and_wait().await?;
        for (phase, context) in events {
            telemetry.log_info(phase, context)?;
        }
        telemetry.shutdown().await;
        Ok::<(), anyhow::Error>(())
    })
}
