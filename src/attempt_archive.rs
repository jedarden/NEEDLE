//! Attempt archive spool producer: one bundle per resolved attempt, handed to
//! a local spool directory for an external drain.
//!
//! The `attempt_archive` config section, the spool layout contract and the
//! drain (`agent-transcript-archive/scripts/spool_drain.py`, run by the
//! `needle-attempt-archive-drain` timer) all existed before this module — but
//! nothing ever wrote a bundle, so the drain had uploaded zero bytes and the
//! durable off-host copy of every attempt that plan section 4.4 step 1 calls
//! for did not exist. This is the writer.
//!
//! Layout, as the drain validates it:
//!
//! ```text
//! <spool>/<host>/<YYYY-MM-DD>/<bead>-<attempt>.tar.zst   the bundle
//! <spool>/<host>/<YYYY-MM-DD>/<bead>-<attempt>.json      the sidecar (commit marker)
//! ```
//!
//! The sidecar carries `bundle_path` (relative to the spool), `bundle_sha256`
//! and `bundle_bytes`, plus the attempt facts. It is written last, through a
//! `.partial` temp file and a rename, so the drain never sees a sidecar
//! without its complete bundle. NEEDLE never uploads, never holds a sink
//! credential and never deletes from the spool; the drain owns all of that.
//!
//! Bundles are produced with the system `tar` (and `zstd` when configured
//! and present) rather than a compression crate: both are on every fleet
//! host, and shelling out keeps the archive independent of the binary's
//! feature set.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{ArchiveCompression, AttemptArchiveConfig};
use crate::sanitize::{CustomPattern, Sanitizer};
use crate::trace::{HarnessTranscriptStatus, TraceFormat};

/// Schema version stamped on every sidecar.
pub const SIDECAR_SCHEMA_VERSION: u32 = 1;

/// The facts an attempt is archived with.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttemptArchiveInput {
    pub attempt_id: String,
    pub bead_id: String,
    pub workspace: String,
    pub worker: String,
    pub adapter: String,
    /// Compatibility alias for the adapter identifier requested.
    #[serde(default)]
    pub model: Option<String>,
    /// Adapter model identifier requested for this attempt.
    #[serde(default)]
    pub requested_model: Option<String>,
    /// Model identifier returned by provider metadata; unknown remains absent.
    #[serde(default)]
    pub effective_model: Option<String>,
    /// Provider response metadata field that supplied `effective_model`.
    #[serde(default)]
    pub model_resolution_source: Option<String>,
    pub outcome: String,
    #[serde(default)]
    pub terminal_reason: Option<String>,
    pub recorded_at: String,
}

/// The sidecar the drain reads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sidecar {
    pub schema_version: u32,
    /// Bundle path relative to the spool root.
    pub bundle_path: String,
    pub bundle_sha256: String,
    pub bundle_bytes: u64,
    pub host: String,
    pub needle_version: String,
    #[serde(flatten)]
    pub attempt: AttemptArchiveInput,
    /// Files included in the bundle, relative to its root.
    #[serde(default)]
    pub files: Vec<String>,
}

/// Where a spooled attempt landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpoolReceipt {
    pub bundle: PathBuf,
    pub sidecar: PathBuf,
    pub bundle_bytes: u64,
}

/// Spool one resolved attempt. `trace_dir` is the attempt's trace directory
/// (`<workspace>/.beads/traces/<bead>/`); a missing or empty one still
/// produces a bundle carrying `attempt.json`, so the archive has a row for
/// every attempt, not only the ones with a transcript.
///
/// Returns `Ok(None)` when the archive is disabled.
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
    spool_attempt_with_sanitizer(config, input, trace_dir, sanitizer)
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

/// Spool one resolved attempt, optionally sanitizing harness files before they
/// enter the bundle.
pub fn spool_attempt_with_sanitizer(
    config: &AttemptArchiveConfig,
    input: &AttemptArchiveInput,
    trace_dir: Option<&Path>,
    sanitizer: Option<Arc<Sanitizer>>,
) -> Result<Option<SpoolReceipt>> {
    spool_attempt_with_sanitizer_and_claude_dir(config, input, trace_dir, sanitizer, None)
}

fn spool_attempt_with_sanitizer_and_claude_dir(
    config: &AttemptArchiveConfig,
    input: &AttemptArchiveInput,
    trace_dir: Option<&Path>,
    sanitizer: Option<Arc<Sanitizer>>,
    claude_dir: Option<&Path>,
) -> Result<Option<SpoolReceipt>> {
    if !config.enabled {
        return Ok(None);
    }
    // ADR-030 decision 5 (N-T52): while a state-root override is in force the
    // spool relocates beneath it, so a fixture suite spools nothing into an
    // operator's configured directory. Without an override the configured
    // `attempt_archive.spool_dir` decides, as before.
    let spool = crate::state_dir::spool_dir_under_override().unwrap_or_else(|| {
        PathBuf::from(crate::util::expand_tilde(
            &config.spool_dir.to_string_lossy(),
        ))
    });
    let host = gethostname::gethostname().to_string_lossy().to_string();
    let day = input
        .recorded_at
        .get(..10)
        .filter(|d| d.len() == 10)
        .map(str::to_string)
        .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());
    let stem = format!(
        "{}-{}",
        safe_component(&input.bead_id),
        safe_component(&input.attempt_id)
    );
    let day_dir = spool.join(safe_component(&host)).join(&day);
    std::fs::create_dir_all(&day_dir)
        .with_context(|| format!("failed to create spool directory {}", day_dir.display()))?;

    // Stage the bundle contents. The staging root is under the spool so the
    // final rename is on one filesystem; its name never ends in .json, so
    // the drain ignores it.
    let staging = spool.join(".staging").join(&stem);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("failed to create staging dir {}", staging.display()))?;
    let result = stage_and_pack(
        config,
        input,
        trace_dir,
        &staging,
        &day_dir,
        &stem,
        &spool,
        &host,
        sanitizer.as_deref(),
        claude_dir,
    );
    let _ = std::fs::remove_dir_all(&staging);
    result.map(Some)
}

#[allow(clippy::too_many_arguments)]
fn stage_and_pack(
    config: &AttemptArchiveConfig,
    input: &AttemptArchiveInput,
    trace_dir: Option<&Path>,
    staging: &Path,
    day_dir: &Path,
    stem: &str,
    spool: &Path,
    host: &str,
    sanitizer: Option<&Sanitizer>,
    claude_dir: Option<&Path>,
) -> Result<SpoolReceipt> {
    let mut files: Vec<String> = Vec::new();

    std::fs::write(
        staging.join("attempt.json"),
        serde_json::to_string_pretty(input)?,
    )
    .context("failed to write attempt.json")?;
    files.push("attempt.json".to_string());

    if let Some(dir) = trace_dir.filter(|d| d.is_dir()) {
        if config.include.harness_transcript {
            let status = copy_harness_transcript(
                input,
                dir,
                &staging.join("harness"),
                sanitizer,
                claude_dir,
            )?;
            update_harness_status(dir, status)?;
            if staging.join("harness").is_dir() {
                files.extend(files_below(staging, &staging.join("harness"))?);
            }
        }

        let mut wanted: Vec<&str> = vec!["metadata.json", "attempts.jsonl"];
        if config.include.trace {
            wanted.extend(["stdout.txt", "stderr.txt", "trace.jsonl"]);
        }
        if config.include.prompt {
            wanted.push("prompt.md");
        }
        for name in wanted {
            let source = dir.join(name);
            if source.is_file() {
                std::fs::copy(&source, staging.join(name))
                    .with_context(|| format!("failed to stage {}", source.display()))?;
                files.push(name.to_string());
            }
        }
    }

    // tar the staging root (relative paths only).
    let tar_path = day_dir.join(format!("{stem}.tar"));
    let status = Command::new("tar")
        .arg("-cf")
        .arg(&tar_path)
        .arg("-C")
        .arg(staging)
        .arg(".")
        .status()
        .context("failed to run tar")?;
    if !status.success() {
        let _ = std::fs::remove_file(&tar_path);
        bail!("tar exited with {status}");
    }

    let bundle = match config.compression {
        ArchiveCompression::Zstd if which_exists("zstd") => {
            let zst = day_dir.join(format!("{stem}.tar.zst"));
            let status = Command::new("zstd")
                .args(["-q", "-f", "--rm", "-o"])
                .arg(&zst)
                .arg(&tar_path)
                .status()
                .context("failed to run zstd")?;
            if !status.success() {
                let _ = std::fs::remove_file(&zst);
                let _ = std::fs::remove_file(&tar_path);
                bail!("zstd exited with {status}");
            }
            zst
        }
        _ => tar_path,
    };

    let (bundle_sha256, bundle_bytes) = sha256_file(&bundle)?;
    let sidecar_path = day_dir.join(format!("{stem}.json"));
    let sidecar = Sidecar {
        schema_version: SIDECAR_SCHEMA_VERSION,
        bundle_path: bundle
            .strip_prefix(spool)
            .unwrap_or(&bundle)
            .to_string_lossy()
            .to_string(),
        bundle_sha256,
        bundle_bytes,
        host: host.to_string(),
        needle_version: env!("CARGO_PKG_VERSION").to_string(),
        attempt: input.clone(),
        files,
    };
    // The sidecar is the commit marker: written whole, then renamed.
    let partial = day_dir.join(format!(".{stem}.json.{}.partial", std::process::id()));
    std::fs::write(&partial, serde_json::to_string(&sidecar)?)
        .with_context(|| format!("failed to write {}", partial.display()))?;
    std::fs::rename(&partial, &sidecar_path)
        .with_context(|| format!("failed to commit {}", sidecar_path.display()))?;

    Ok(SpoolReceipt {
        bundle,
        sidecar: sidecar_path,
        bundle_bytes,
    })
}

/// Copy the Claude session transcript and any subagent sidecars into the
/// bundle staging directory. A missing session is an expected retention race,
/// not a spool error; the caller records it in metadata instead.
fn copy_harness_transcript(
    input: &AttemptArchiveInput,
    trace_dir: &Path,
    destination: &Path,
    sanitizer: Option<&Sanitizer>,
    claude_dir: Option<&Path>,
) -> Result<HarnessTranscriptStatus> {
    let metadata_path = trace_dir.join("metadata.json");
    let metadata = match std::fs::read_to_string(&metadata_path) {
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
        crate::transcript::TranscriptDiscovery::new(Path::new(&input.workspace), claude_dir, 0);
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

    std::fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    let session_destination = destination.join(format!("{session_id}.jsonl"));
    copy_sanitized_file(&session_path, &session_destination, sanitizer)?;

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
    let bytes = std::fs::read(source)
        .with_context(|| format!("failed to read harness transcript {}", source.display()))?;
    let bytes = match String::from_utf8(bytes) {
        Ok(text) => sanitizer
            .map(|sanitizer| sanitizer.sanitize(&text))
            .unwrap_or(text)
            .into_bytes(),
        Err(error) => error.into_bytes(),
    };
    std::fs::write(destination, bytes)
        .with_context(|| format!("failed to write {}", destination.display()))
}

fn copy_sanitized_tree(
    source: &Path,
    destination: &Path,
    sanitizer: Option<&Sanitizer>,
) -> Result<()> {
    std::fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
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
        &std::fs::read_to_string(&metadata_path)
            .with_context(|| format!("failed to read {}", metadata_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", metadata_path.display()))?;
    metadata["harness_transcript"] = serde_json::to_value(status)?;
    std::fs::write(&metadata_path, serde_json::to_string_pretty(&metadata)?).with_context(|| {
        format!(
            "failed to write harness transcript status to {}",
            metadata_path.display()
        )
    })
}

fn files_below(root: &Path, directory: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)
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

fn which_exists(binary: &str) -> bool {
    which::which(binary).is_ok()
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

/// SHA-256 hex digest and byte size of a file.
pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
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
}
