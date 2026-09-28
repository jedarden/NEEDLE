//! Attempt archive spool producer: one bundle per resolved attempt, handed to
//! a local spool directory for an external drain.
//!
//! The spool is deliberately the only sink-facing contract. A complete bundle
//! is published first, and its JSON sidecar is published last; a drain must
//! ignore bundles that do not have a sidecar.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{ArchiveCompression, AttemptArchiveConfig};
use crate::sanitize::{CustomPattern, Sanitizer};
use crate::trace::{HarnessTranscriptStatus, TraceFormat};

pub const SIDECAR_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttemptArchiveInput {
    pub attempt_id: String,
    pub bead_id: String,
    pub workspace: String,
    pub worker: String,
    pub adapter: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub requested_model: Option<String>,
    #[serde(default)]
    pub effective_model: Option<String>,
    #[serde(default)]
    pub model_resolution_source: Option<String>,
    pub outcome: String,
    #[serde(default)]
    pub terminal_reason: Option<String>,
    pub recorded_at: String,
}

/// Resolved fields supplied by the finalized attempt ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttemptArchiveMetadata {
    pub provisional: bool,
    pub requested_action: Option<String>,
}

/// Version-1 sidecar consumed by the external spool drain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sidecar {
    pub schema_version: u32,
    pub bundle_path: String,
    pub bundle_sha256: String,
    pub bundle_bytes: u64,
    pub host: String,
    pub workspace: String,
    pub workspace_slug: String,
    pub bead_id: String,
    pub attempt_id: String,
    pub provisional: bool,
    pub session_id: Option<String>,
    pub adapter: String,
    pub model: Option<String>,
    pub worker_id: String,
    pub outcome: String,
    pub requested_action: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub contents: Vec<String>,
}

/// Marker stored beside an attempt trace after the spool pair is complete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpooledReceipt {
    pub bundle_path: String,
    pub sha256: String,
    pub spooled_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpoolReceipt {
    pub bundle: PathBuf,
    pub sidecar: PathBuf,
    pub bundle_sha256: String,
    pub bundle_bytes: u64,
}

/// Spool one resolved attempt. A disabled archive returns without touching the
/// configured spool or the trace directory.
pub fn spool_attempt(
    config: &AttemptArchiveConfig,
    input: &AttemptArchiveInput,
    trace_dir: Option<&Path>,
) -> Result<Option<SpoolReceipt>> {
    let sanitizer = if config.enabled && config.include.harness_transcript {
        Some(Arc::new(Sanitizer::new(&[])?))
    } else {
        None
    };
    spool_attempt_with_metadata(
        config,
        input,
        trace_dir,
        &AttemptArchiveMetadata::default(),
        sanitizer,
    )
}

/// Build the same sanitizer configuration used by dispatch trace capture.
pub fn configured_sanitizer(config: &crate::config::Config) -> Result<Option<Arc<Sanitizer>>> {
    if !config.attempt_archive.enabled
        || !config.attempt_archive.include.harness_transcript
        || !config.strands.learning.trace_sanitization.enabled
    {
        return Ok(None);
    }
    let custom_patterns = config
        .strands
        .learning
        .trace_sanitization
        .custom_patterns
        .iter()
        .map(|pattern| CustomPattern {
            id: pattern.id.clone(),
            pattern: pattern.pattern.clone(),
            entropy: pattern.entropy,
        })
        .collect::<Vec<_>>();
    Ok(Some(Arc::new(Sanitizer::new(&custom_patterns)?)))
}

pub fn spool_attempt_with_sanitizer(
    config: &AttemptArchiveConfig,
    input: &AttemptArchiveInput,
    trace_dir: Option<&Path>,
    sanitizer: Option<Arc<Sanitizer>>,
) -> Result<Option<SpoolReceipt>> {
    spool_attempt_with_metadata(
        config,
        input,
        trace_dir,
        &AttemptArchiveMetadata::default(),
        sanitizer,
    )
}

/// Spool an attempt with finalized ledger fields needed by the sidecar.
pub fn spool_attempt_with_metadata(
    config: &AttemptArchiveConfig,
    input: &AttemptArchiveInput,
    trace_dir: Option<&Path>,
    archive_metadata: &AttemptArchiveMetadata,
    sanitizer: Option<Arc<Sanitizer>>,
) -> Result<Option<SpoolReceipt>> {
    if !config.enabled {
        return Ok(None);
    }
    let spool = crate::state_dir::spool_dir_under_override().unwrap_or_else(|| {
        PathBuf::from(crate::util::expand_tilde(
            &config.spool_dir.to_string_lossy(),
        ))
    });
    let host = gethostname::gethostname().to_string_lossy().to_string();
    let attempt = safe_component_or_unknown(&input.attempt_id);
    let attempt_dir = spool
        .join(safe_component_or_unknown(&host))
        .join(workspace_slug(&input.workspace))
        .join(safe_component_or_unknown(&input.bead_id));
    fs::create_dir_all(&attempt_dir)
        .with_context(|| format!("failed to create spool directory {}", attempt_dir.display()))?;

    let staging = tempfile::Builder::new()
        .prefix(&format!(".{attempt}-"))
        .tempdir_in(&attempt_dir)
        .with_context(|| {
            format!(
                "failed to create archive staging directory {}",
                attempt_dir.display()
            )
        })?;
    let receipt = stage_and_pack(
        config,
        input,
        trace_dir,
        archive_metadata,
        staging.path(),
        &attempt_dir,
        &spool,
        &host,
        sanitizer.as_deref(),
    )?;
    if let Some(trace_dir) = trace_dir.filter(|path| path.is_dir()) {
        write_spooled_receipt(trace_dir, &receipt)?;
    }
    Ok(Some(receipt))
}

#[allow(clippy::too_many_arguments)]
fn stage_and_pack(
    config: &AttemptArchiveConfig,
    input: &AttemptArchiveInput,
    trace_dir: Option<&Path>,
    archive_metadata: &AttemptArchiveMetadata,
    staging: &Path,
    attempt_dir: &Path,
    spool: &Path,
    host: &str,
    sanitizer: Option<&Sanitizer>,
) -> Result<SpoolReceipt> {
    let mut contents = Vec::new();
    if let Some(dir) = trace_dir.filter(|d| d.is_dir()) {
        if config.include.harness_transcript {
            let status = copy_harness_transcript(input, dir, &staging.join("harness"), sanitizer)?;
            update_harness_status(dir, status)?;
            if staging.join("harness").is_dir() {
                contents.extend(files_below(staging, &staging.join("harness"))?);
            }
        }
        if config.include.trace {
            for name in ["stdout.txt", "stderr.txt", "trace.jsonl", "metadata.json"] {
                let source = dir.join(name);
                if source.is_file() {
                    let archive_name = Path::new("trace").join(name);
                    let destination = staging.join(&archive_name);
                    fs::create_dir_all(destination.parent().unwrap_or(staging))?;
                    fs::copy(&source, &destination)
                        .with_context(|| format!("failed to stage {}", source.display()))?;
                    contents.push(archive_name.to_string_lossy().to_string());
                }
            }
        }
        if config.include.prompt {
            let source = dir.join("prompt.md");
            if source.is_file() {
                fs::copy(&source, staging.join("prompt.md"))
                    .with_context(|| format!("failed to stage {}", source.display()))?;
                contents.push("prompt.md".to_string());
            }
        }
    }
    contents.sort();

    let facts = trace_facts(trace_dir);
    let attempt = safe_component_or_unknown(&input.attempt_id);
    let suffix = match config.compression {
        ArchiveCompression::Zstd => ".tar.zst",
        ArchiveCompression::None => ".tar",
    };
    let bundle = attempt_dir.join(format!("{attempt}{suffix}"));
    let bundle_partial = attempt_dir.join(format!("{attempt}{suffix}.partial"));
    write_bundle(staging, &contents, &bundle_partial, config.compression)?;
    fs::rename(&bundle_partial, &bundle)
        .with_context(|| format!("failed to publish archive bundle {}", bundle.display()))?;
    sync_directory(attempt_dir)?;

    let (bundle_sha256, bundle_bytes) = sha256_file(&bundle)?;
    let sidecar_path = attempt_dir.join(format!("{attempt}.json"));
    let sidecar = Sidecar {
        schema_version: SIDECAR_SCHEMA_VERSION,
        bundle_path: bundle
            .strip_prefix(spool)
            .unwrap_or(&bundle)
            .to_string_lossy()
            .to_string(),
        bundle_sha256: bundle_sha256.clone(),
        bundle_bytes,
        host: host.to_string(),
        workspace: input.workspace.clone(),
        workspace_slug: workspace_slug(&input.workspace),
        bead_id: input.bead_id.clone(),
        attempt_id: input.attempt_id.clone(),
        provisional: archive_metadata.provisional,
        session_id: facts.session_id,
        adapter: input.adapter.clone(),
        model: input
            .model
            .clone()
            .or_else(|| input.requested_model.clone())
            .or(facts.effective_model)
            .or_else(|| input.effective_model.clone()),
        worker_id: input.worker.clone(),
        outcome: input.outcome.clone(),
        requested_action: facts
            .requested_action
            .or_else(|| archive_metadata.requested_action.clone()),
        started_at: facts.started_at.or_else(|| Some(input.recorded_at.clone())),
        finished_at: facts
            .finished_at
            .or_else(|| Some(input.recorded_at.clone())),
        contents,
    };
    write_atomic_json(&sidecar_path, &sidecar)?;

    Ok(SpoolReceipt {
        bundle,
        sidecar: sidecar_path,
        bundle_sha256,
        bundle_bytes,
    })
}

#[derive(Default)]
struct TraceFacts {
    session_id: Option<String>,
    effective_model: Option<String>,
    requested_action: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

fn trace_facts(trace_dir: Option<&Path>) -> TraceFacts {
    let Some(path) = trace_dir.map(|dir| dir.join("metadata.json")) else {
        return TraceFacts::default();
    };
    let Ok(bytes) = fs::read(path) else {
        return TraceFacts::default();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return TraceFacts::default();
    };
    let string = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    TraceFacts {
        session_id: string("session_id"),
        effective_model: string("effective_model"),
        requested_action: string("requested_action"),
        started_at: string("started_at"),
        finished_at: string("finished_at"),
    }
}

fn write_bundle(
    staging: &Path,
    contents: &[String],
    partial: &Path,
    compression: ArchiveCompression,
) -> Result<()> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(partial)
        .with_context(|| format!("failed to create partial bundle {}", partial.display()))?;
    match compression {
        ArchiveCompression::None => {
            let mut builder = tar::Builder::new(file);
            append_staged_contents(&mut builder, staging, contents)?;
            let file = builder
                .into_inner()
                .context("failed to finish tar bundle")?;
            file.sync_all().context("failed to fsync tar bundle")?;
        }
        ArchiveCompression::Zstd => {
            let encoder = zstd::Encoder::new(file, 0).context("failed to create zstd encoder")?;
            let mut builder = tar::Builder::new(encoder);
            append_staged_contents(&mut builder, staging, contents)?;
            let encoder = builder
                .into_inner()
                .context("failed to finish tar before zstd")?;
            let file = encoder.finish().context("failed to finish zstd bundle")?;
            file.sync_all().context("failed to fsync zstd bundle")?;
        }
    }
    Ok(())
}

fn append_staged_contents<W: Write>(
    builder: &mut tar::Builder<W>,
    staging: &Path,
    contents: &[String],
) -> Result<()> {
    for relative in contents {
        let source = staging.join(relative);
        builder
            .append_path_with_name(&source, relative)
            .with_context(|| format!("failed to add {} to archive", relative))?;
    }
    Ok(())
}

fn write_atomic_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_atomic_bytes(path, &bytes)
}

fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let partial = PathBuf::from(format!("{}.partial", path.display()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&partial)
        .with_context(|| format!("failed to create {}", partial.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write {}", partial.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to fsync {}", partial.display()))?;
    drop(file);
    fs::rename(&partial, path).with_context(|| format!("failed to publish {}", path.display()))?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn write_spooled_receipt(trace_dir: &Path, receipt: &SpoolReceipt) -> Result<()> {
    let marker = trace_dir.join("spooled.json");
    let value = SpooledReceipt {
        bundle_path: receipt.bundle.display().to_string(),
        sha256: receipt.bundle_sha256.clone(),
        spooled_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    };
    write_atomic_json(&marker, &value)
}

/// Return the final component of a workspace path as a safe spool directory.
pub fn workspace_slug(workspace: &str) -> String {
    let trimmed = workspace.trim_end_matches(['/', '\\']);
    let name = trimmed
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
        .unwrap_or("unknown");
    safe_component_or_unknown(name)
}

fn safe_component_or_unknown(value: &str) -> String {
    let component = safe_component(value);
    if component.is_empty() {
        "unknown".to_string()
    } else {
        component
    }
}

fn safe_component(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open directory {} for fsync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to fsync directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn copy_harness_transcript(
    input: &AttemptArchiveInput,
    trace_dir: &Path,
    destination: &Path,
    sanitizer: Option<&Sanitizer>,
) -> Result<HarnessTranscriptStatus> {
    let metadata_path = trace_dir.join("metadata.json");
    let metadata = match fs::read_to_string(&metadata_path) {
        Ok(metadata) => serde_json::from_str::<serde_json::Value>(&metadata)
            .with_context(|| format!("failed to parse {}", metadata_path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(
                attempt_id = %input.attempt_id,
                "harness transcript absent: attempt metadata is unavailable"
            );
            return Ok(HarnessTranscriptStatus::Absent);
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read {}", metadata_path.display()))
        }
    };

    let adapter = metadata
        .get("adapter")
        .or_else(|| metadata.get("agent"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if !matches!(
        crate::trace::detect_trace_format(adapter),
        TraceFormat::ClaudeJson
    ) {
        tracing::debug!(
            attempt_id = %input.attempt_id,
            adapter,
            "harness transcript unsupported for adapter"
        );
        return Ok(HarnessTranscriptStatus::UnsupportedAdapter);
    }
    let session_id = metadata
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty());
    let Some(session_id) = session_id else {
        tracing::debug!(
            attempt_id = %input.attempt_id,
            "harness transcript absent: session id is unavailable"
        );
        return Ok(HarnessTranscriptStatus::Absent);
    };

    let discovery =
        crate::transcript::TranscriptDiscovery::new(Path::new(&input.workspace), None, 0);
    let Some(session_path) = discovery.session_path(session_id) else {
        tracing::debug!(
            attempt_id = %input.attempt_id,
            "harness transcript absent: session id is not a safe path component"
        );
        return Ok(HarnessTranscriptStatus::Absent);
    };
    if !session_path.is_file() {
        tracing::debug!(
            attempt_id = %input.attempt_id,
            session_id,
            path = %session_path.display(),
            "harness transcript absent: session file not found"
        );
        return Ok(HarnessTranscriptStatus::Absent);
    }

    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    copy_sanitized_file(
        &session_path,
        &destination.join(format!("{session_id}.jsonl")),
        sanitizer,
    )?;
    if let Some(subagent_dir) = discovery.session_subagent_dir(session_id) {
        if subagent_dir.is_dir() {
            copy_sanitized_tree(&subagent_dir, &destination.join(session_id), sanitizer)?;
        }
    }
    Ok(HarnessTranscriptStatus::Present)
}

fn copy_sanitized_file(
    source: &Path,
    destination: &Path,
    sanitizer: Option<&Sanitizer>,
) -> Result<()> {
    let bytes = fs::read(source)
        .with_context(|| format!("failed to read harness transcript {}", source.display()))?;
    let bytes = match String::from_utf8(bytes) {
        Ok(text) => sanitizer
            .map(|sanitizer| sanitizer.sanitize(&text))
            .unwrap_or(text)
            .into_bytes(),
        Err(error) => error.into_bytes(),
    };
    fs::write(destination, bytes).with_context(|| {
        format!(
            "failed to write harness transcript {}",
            destination.display()
        )
    })
}

fn copy_sanitized_tree(
    source: &Path,
    destination: &Path,
    sanitizer: Option<&Sanitizer>,
) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_sanitized_tree(&source_path, &destination_path, sanitizer)?;
        } else if file_type.is_file() {
            copy_sanitized_file(&source_path, &destination_path, sanitizer)?;
        } else {
            tracing::debug!(
                path = %source_path.display(),
                "skipping non-regular harness sidecar"
            );
        }
    }
    Ok(())
}

fn update_harness_status(trace_dir: &Path, status: HarnessTranscriptStatus) -> Result<()> {
    let metadata_path = trace_dir.join("metadata.json");
    if !metadata_path.is_file() {
        return Ok(());
    }
    let mut metadata = serde_json::from_str::<serde_json::Value>(
        &fs::read_to_string(&metadata_path)
            .with_context(|| format!("failed to read {}", metadata_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", metadata_path.display()))?;
    metadata["harness_transcript"] = serde_json::to_value(status)?;
    fs::write(&metadata_path, serde_json::to_string_pretty(&metadata)?).with_context(|| {
        format!(
            "failed to write harness transcript status to {}",
            metadata_path.display()
        )
    })
}

fn files_below(root: &Path, directory: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            files.extend(files_below(root, &path)?);
        } else if entry.file_type()?.is_file() {
            files.push(
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }
    files.sort();
    Ok(files)
}

pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), size))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> AttemptArchiveInput {
        AttemptArchiveInput {
            attempt_id: "0192-attempt-1".into(),
            bead_id: "needle-abc".into(),
            workspace: "/ws".into(),
            worker: "needle-alpha".into(),
            adapter: "claude-code-glm-5.3-flash".into(),
            model: Some("glm-5.3-flash".into()),
            requested_model: Some("glm-5.3-flash".into()),
            effective_model: Some("glm-5.3-flash".into()),
            model_resolution_source: Some("claude_message.model".into()),
            outcome: "work_failure".into(),
            terminal_reason: Some("gate:dod".into()),
            recorded_at: "2026-09-12T15:00:00.000Z".into(),
        }
    }

    #[test]
    fn disabled_archive_spools_nothing() {
        let config = AttemptArchiveConfig::default();
        assert!(spool_attempt(&config, &input(), None).unwrap().is_none());
    }

    #[test]
    fn workspace_slug_uses_the_final_path_component() {
        assert_eq!(workspace_slug("/home/coding/NEEDLE"), "NEEDLE");
        assert_eq!(workspace_slug("/home/coding/project@v1"), "project_v1");
        assert_eq!(workspace_slug("/"), "unknown");
    }
}
