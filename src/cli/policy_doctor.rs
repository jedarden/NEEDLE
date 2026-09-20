//! The read-only policy doctor report.
//!
//! Policy sources are collected at the same boundaries used by dispatch:
//! configuration, instruction files, and configured prompt material. The
//! resolver remains the authority for ordering and conflict detection; this
//! module only turns its result into an operator-facing report.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::lesson;
use crate::config::{CliOverrides, Config, ConfigLoader};
use crate::policy::{
    Authority, AuthorityRegistry, PolicyError, PolicyKind, PolicyScope, PolicySource,
    ResolutionContext, ResolvedPolicy,
};

const REPORT_SCHEMA_VERSION: u8 = 1;

/// Machine-readable policy provenance and conflict report.
#[derive(Debug, Serialize)]
pub(crate) struct Report {
    pub schema_version: u8,
    pub workspace: String,
    pub adapter: String,
    pub status: &'static str,
    pub provenance: Vec<SourceReport>,
    pub conflicts: Vec<ConflictReport>,
    pub expired_lessons: Vec<ExpiredLessonReport>,
    pub manifest: Option<ManifestReport>,
    pub exit_code: u8,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct SourceReport {
    pub id: String,
    pub path: String,
    pub kind: String,
    pub authority: String,
    pub precedence_rank: u8,
    pub scope: String,
    pub content_sha256: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ConflictReport {
    pub authority: String,
    pub scope: String,
    pub source_ids: Vec<String>,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExpiredLessonReport {
    pub id: String,
    pub path: String,
    pub expired_at: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ManifestReport {
    pub version: &'static str,
    pub hash: String,
    pub source_ids: Vec<String>,
}

impl Report {
    pub(crate) fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }

    pub(crate) fn has_findings(&self) -> bool {
        self.has_conflicts() || !self.expired_lessons.is_empty()
    }

    pub(crate) fn render_human(&self) -> String {
        let mut output = String::new();
        output.push_str("NEEDLE Policy Doctor\n");
        output.push_str(&format!("Workspace: {}\n", self.workspace));
        output.push_str(&format!("Adapter: {}\n", self.adapter));
        output.push_str(&format!("Status: {}\n", self.status.to_uppercase()));

        output.push_str("Provenance:\n");
        for source in &self.provenance {
            output.push_str(&format!(
                "  - {} [{} rank={} scope={}]\n",
                source.path, source.authority, source.precedence_rank, source.scope
            ));
            output.push_str(&format!("    sha256: {}\n", source.content_sha256));
        }

        if let Some(manifest) = &self.manifest {
            output.push_str(&format!(
                "Manifest: {} ({})\n",
                manifest.hash, manifest.version
            ));
        } else {
            output.push_str("Manifest: unavailable because policy resolution failed\n");
        }

        if self.conflicts.is_empty() {
            output.push_str("Conflicts: none\n");
        } else {
            output.push_str("Conflicts:\n");
            for conflict in &self.conflicts {
                output.push_str(&format!(
                    "  - {} at {}: {}\n",
                    conflict.authority, conflict.scope, conflict.detail
                ));
                output.push_str(&format!(
                    "    sources: {}\n",
                    conflict.source_ids.join(", ")
                ));
            }
        }
        if self.expired_lessons.is_empty() {
            output.push_str("Expired lessons: none\n");
        } else {
            output.push_str("Expired lessons:\n");
            for lesson in &self.expired_lessons {
                output.push_str(&format!(
                    "  - {} at {} (expired {})\n",
                    lesson.id, lesson.path, lesson.expired_at
                ));
            }
        }
        output
    }
}

/// Collect the policy sources applicable to a workspace and resolve them.
pub(crate) fn collect(workspace: PathBuf, adapter: Option<String>) -> Result<Report> {
    ConfigLoader::load_global().context("load global config for policy doctor")?;
    let (config, _) = ConfigLoader::load_resolved(
        &workspace,
        CliOverrides {
            workspace: Some(workspace.clone()),
            ..Default::default()
        },
    )?;
    let adapter = adapter.unwrap_or_else(|| config.agent.default.clone());

    let mut sources = Vec::new();
    let mut source_ids = BTreeSet::new();

    let global_config = global_config_path();
    if !global_config.is_file() {
        add_builtin_source(&mut sources, &mut source_ids)?;
    }
    add_file_source(
        &mut sources,
        &mut source_ids,
        &global_config,
        PolicyKind::ExecutableGate,
        PolicyScope::Global,
    )?;

    let workspace_config = workspace.join(".needle.yaml");
    add_file_source(
        &mut sources,
        &mut source_ids,
        &workspace_config,
        PolicyKind::ExecutableGate,
        PolicyScope::repository(&workspace),
    )?;

    add_instruction_sources(&workspace, &mut sources, &mut source_ids)?;
    add_configured_context_files(&workspace, &config, &mut sources, &mut source_ids)?;
    add_promotion_targets(&workspace, &mut sources, &mut source_ids)?;
    add_configured_prompt_sources(&config, &adapter, &mut sources, &mut source_ids)?;

    let now = Utc::now();
    let expired_lessons = sources
        .iter()
        .filter(|source| {
            matches!(
                source.kind,
                PolicyKind::RepositoryInstructions | PolicyKind::AdapterInstructions
            )
        })
        .flat_map(|source| {
            lesson::expired_markers(&source.content, now)
                .into_iter()
                .map(|expired| ExpiredLessonReport {
                    id: expired.id,
                    path: source.id.as_str().to_owned(),
                    expired_at: expired.expired_at.to_rfc3339(),
                })
        })
        .collect::<Vec<_>>();

    let mut registry = AuthorityRegistry::new();
    for source in &sources {
        registry
            .insert(source.clone())
            .with_context(|| format!("register policy source {}", source.id.as_str()))?;
    }

    let context = ResolutionContext::new(&workspace).for_adapter(&adapter);
    let resolved = registry.resolve(&context);
    let provenance = match &resolved {
        Ok(policy) => policy
            .sources()
            .iter()
            .map(|source| SourceReport::from_source(source))
            .collect(),
        Err(_) => sources.iter().map(SourceReport::from_source).collect(),
    };

    let (conflicts, manifest, status, exit_code) = match resolved {
        Ok(policy) if expired_lessons.is_empty() => {
            (Vec::new(), Some(manifest_from(&policy)?), "pass", 0)
        }
        Ok(policy) => (Vec::new(), Some(manifest_from(&policy)?), "warn", 1),
        Err(error) => (vec![conflict_from(error)], None, "fail", 1),
    };

    Ok(Report {
        schema_version: REPORT_SCHEMA_VERSION,
        workspace: workspace.display().to_string(),
        adapter,
        status,
        provenance,
        conflicts,
        expired_lessons,
        manifest,
        exit_code,
    })
}

fn global_config_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".config/needle/config.yaml")
}

fn add_builtin_source(
    sources: &mut Vec<PolicySource>,
    source_ids: &mut BTreeSet<String>,
) -> Result<()> {
    add_source(
        sources,
        source_ids,
        "builtin:defaults",
        PolicyKind::ExecutableGate,
        PolicyScope::Global,
        "needle built-in policy defaults",
    )
}

fn add_file_source(
    sources: &mut Vec<PolicySource>,
    source_ids: &mut BTreeSet<String>,
    path: &Path,
    kind: PolicyKind,
    scope: PolicyScope,
) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("read policy source {}", path.display()))?;
    add_source(
        sources,
        source_ids,
        &path.display().to_string(),
        kind,
        scope,
        content,
    )
}

fn add_source(
    sources: &mut Vec<PolicySource>,
    source_ids: &mut BTreeSet<String>,
    id: &str,
    kind: PolicyKind,
    scope: PolicyScope,
    content: impl Into<String>,
) -> Result<()> {
    if !source_ids.insert(id.to_owned()) {
        return Ok(());
    }
    sources.push(PolicySource::new(id, kind, scope, content)?);
    Ok(())
}

fn add_instruction_sources(
    workspace: &Path,
    sources: &mut Vec<PolicySource>,
    source_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let mut directories = Vec::new();
    let mut directory = workspace.to_path_buf();
    loop {
        directories.push(directory.clone());
        let Some(parent) = directory.parent() else {
            break;
        };
        if parent == directory {
            break;
        }
        directory = parent.to_path_buf();
    }

    for directory in directories {
        let mut files = fs::read_dir(&directory)
            .with_context(|| format!("read policy directory {}", directory.display()))?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.is_file()
                    && path.extension().and_then(|extension| extension.to_str()) == Some("md")
                    && path
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .map(|stem| {
                            stem == "AGENTS"
                                || stem.starts_with("AGENTS.")
                                || stem == "CLAUDE"
                                || stem.starts_with("CLAUDE.")
                        })
                        .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        files.sort();

        for path in files {
            let is_agents = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.starts_with("AGENTS"))
                .unwrap_or(false);
            let kind = if is_agents {
                PolicyKind::RepositoryInstructions
            } else {
                PolicyKind::AdapterInstructions
            };
            let scope = if is_agents && directory == workspace {
                PolicyScope::repository(workspace)
            } else {
                PolicyScope::directory(&directory)
            };
            add_file_source(sources, source_ids, &path, kind, scope)?;
        }
    }
    Ok(())
}

fn add_configured_context_files(
    workspace: &Path,
    config: &Config,
    sources: &mut Vec<PolicySource>,
    source_ids: &mut BTreeSet<String>,
) -> Result<()> {
    for configured in &config.prompt.context_files {
        let path = if configured.is_absolute() {
            configured.clone()
        } else {
            workspace.join(configured)
        };
        add_file_source(
            sources,
            source_ids,
            &path,
            PolicyKind::RepositoryInstructions,
            PolicyScope::directory(path.parent().unwrap_or(workspace)),
        )?;
    }
    Ok(())
}

fn add_configured_prompt_sources(
    config: &Config,
    adapter: &str,
    sources: &mut Vec<PolicySource>,
    source_ids: &mut BTreeSet<String>,
) -> Result<()> {
    if let Some(instructions) = &config.prompt.instructions {
        add_source(
            sources,
            source_ids,
            "config:prompt.instructions",
            PolicyKind::AdapterInstructions,
            PolicyScope::adapter(adapter),
            instructions.clone(),
        )?;
    }

    if !config.prompt.templates.is_empty() {
        let content = serde_json::to_string(&config.prompt.templates)
            .context("serialize configured prompt templates")?;
        add_source(
            sources,
            source_ids,
            "config:prompt.templates",
            PolicyKind::AdapterInstructions,
            PolicyScope::adapter(adapter),
            content,
        )?;
    }

    Ok(())
}

/// A promotion may name a docs file that is not conventionally named
/// AGENTS.md or CLAUDE.md. Receipts are the authoritative index for those
/// targets, so include each safe, existing target in policy-doctor scanning.
fn add_promotion_targets(
    workspace: &Path,
    sources: &mut Vec<PolicySource>,
    source_ids: &mut BTreeSet<String>,
) -> Result<()> {
    let directory = workspace.join(".needle/promotions");
    if !directory.is_dir() {
        return Ok(());
    }
    let mut receipts = fs::read_dir(&directory)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    receipts.sort();

    let workspace = workspace.canonicalize()?;
    for receipt in receipts {
        let Ok(content) = fs::read_to_string(&receipt) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        let Some(target) = value.get("target").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let path = workspace.join(target);
        let Ok(path) = path.canonicalize() else {
            continue;
        };
        if !path.starts_with(&workspace) || !path.is_file() {
            continue;
        }
        add_file_source(
            sources,
            source_ids,
            &path,
            PolicyKind::RepositoryInstructions,
            PolicyScope::directory(path.parent().unwrap_or(&workspace)),
        )?;
    }
    Ok(())
}

fn manifest_from(policy: &ResolvedPolicy<'_>) -> Result<ManifestReport> {
    Ok(ManifestReport {
        version: "effective-policy-v1",
        hash: policy.hash()?,
        source_ids: policy
            .sources()
            .iter()
            .map(|source| source.id.as_str().to_owned())
            .collect(),
    })
}

fn conflict_from(error: PolicyError) -> ConflictReport {
    match error {
        PolicyError::Conflict {
            authority,
            scope,
            sources,
        } => ConflictReport {
            authority: authority_label(authority).to_owned(),
            scope: scope_label(&scope),
            source_ids: sources
                .iter()
                .map(|source| source.as_str().to_owned())
                .collect(),
            detail: "equal-authority sources have different content".to_owned(),
        },
        other => ConflictReport {
            authority: "unknown".to_owned(),
            scope: "unknown".to_owned(),
            source_ids: Vec::new(),
            detail: other.to_string(),
        },
    }
}

impl SourceReport {
    fn from_source(source: &PolicySource) -> Self {
        Self {
            id: source.id.as_str().to_owned(),
            path: source.id.as_str().to_owned(),
            kind: kind_label(source.kind).to_owned(),
            authority: authority_label(source.authority()).to_owned(),
            precedence_rank: source.authority().rank(),
            scope: scope_label(&source.scope),
            content_sha256: sha256(source.content.as_bytes()),
        }
    }
}

fn sha256(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn kind_label(kind: PolicyKind) -> &'static str {
    match kind {
        PolicyKind::ExternalConstraint => "external_constraint",
        PolicyKind::RepositoryInstructions => "repository_instructions",
        PolicyKind::AdapterInstructions => "adapter_instructions",
        PolicyKind::ExecutableGate => "executable_gate",
        PolicyKind::AcceptedAdr => "accepted_adr",
        PolicyKind::CurrentPlan => "current_plan",
        PolicyKind::AdvisoryMemory => "advisory_memory",
    }
}

fn authority_label(authority: Authority) -> &'static str {
    match authority {
        Authority::Safety => "safety",
        Authority::Repository => "repository",
        Authority::Adapter => "adapter",
        Authority::Gate => "gate",
        Authority::Adr => "adr",
        Authority::Plan => "plan",
        Authority::Memory => "memory",
    }
}

fn scope_label(scope: &PolicyScope) -> String {
    match scope {
        PolicyScope::Global => "global".to_owned(),
        PolicyScope::Repository { root } => format!("repository:{}", root.display()),
        PolicyScope::Directory { path } => format!("directory:{}", path.display()),
        PolicyScope::Adapter { name } => format!("adapter:{name}"),
    }
}
