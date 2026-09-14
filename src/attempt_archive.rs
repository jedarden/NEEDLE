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

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{ArchiveCompression, AttemptArchiveConfig};

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
    #[serde(default)]
    pub model: Option<String>,
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
    if !config.enabled {
        return Ok(None);
    }
    let spool = PathBuf::from(crate::util::expand_tilde(
        &config.spool_dir.to_string_lossy(),
    ));
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
        config, input, trace_dir, &staging, &day_dir, &stem, &spool, &host,
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
) -> Result<SpoolReceipt> {
    let mut files: Vec<String> = Vec::new();

    std::fs::write(
        staging.join("attempt.json"),
        serde_json::to_string_pretty(input)?,
    )
    .context("failed to write attempt.json")?;
    files.push("attempt.json".to_string());

    if let Some(dir) = trace_dir.filter(|d| d.is_dir()) {
        let mut wanted: Vec<&str> = vec!["metadata.json", "attempts.jsonl"];
        if config.include.trace {
            wanted.extend(["stdout.txt", "stderr.txt"]);
        }
        if config.include.harness_transcript {
            wanted.push("trace.jsonl");
        }
        if config.include.prompt {
            wanted.push("prompt.txt");
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
