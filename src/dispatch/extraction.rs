//! Clean git archive extraction for agent dispatch.
//!
//! This module provides functionality to extract a clean copy of a workspace's
//! committed state using `git archive`, with support for including dispatch
//! commits if they were pushed during the dispatch phase.
//!
//! # Extraction Pattern
//!
//! - Creates a per-dispatch temp directory under `$HOME/scratch/needle-<worker>-<bead_id>-<timestamp>/`
//! - Uses `git archive HEAD | tar -x -C <tmp>` for clean extraction
//! - Handles the case where dispatch pushed commits (includes those if detected)
//! - Cleans up extraction on success, preserves on failure with path in release reason

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

/// Configuration for git archive extraction.
#[derive(Debug, Clone)]
pub struct ExtractionConfig {
    /// Worker ID for temp directory naming.
    pub worker_id: String,
    /// Bead ID for temp directory naming.
    pub bead_id: String,
    /// Optional pre-dispatch HEAD SHA to detect if dispatch pushed commits.
    pub pre_dispatch_head: Option<String>,
    /// Base scratch directory (defaults to `$HOME/scratch`).
    pub scratch_base: Option<PathBuf>,
}

impl ExtractionConfig {
    /// Create a new extraction config.
    pub fn new(worker_id: String, bead_id: String) -> Self {
        Self {
            worker_id,
            bead_id,
            pre_dispatch_head: None,
            scratch_base: None,
        }
    }

    /// Set the pre-dispatch HEAD SHA for commit detection.
    pub fn with_pre_dispatch_head(mut self, head: Option<String>) -> Self {
        self.pre_dispatch_head = head;
        self
    }

    /// Set the base scratch directory.
    pub fn with_scratch_base(mut self, base: PathBuf) -> Self {
        self.scratch_base = Some(base);
        self
    }
}

/// Result of a git archive extraction operation.
#[derive(Debug)]
pub struct ExtractionResult {
    /// Path to the extracted clean workspace.
    pub extraction_path: PathBuf,
    /// Whether extraction included dispatch-pushed commits.
    pub included_dispatch_commits: bool,
    /// The HEAD SHA that was extracted (for verification).
    pub extracted_head_sha: String,
}

/// Extract a clean copy of the workspace's committed state using git archive.
///
/// This function:
/// 1. Creates a per-dispatch temp directory under scratch
/// 2. Runs `git archive HEAD | tar -x -C <tmp>` for clean extraction
/// 3. Handles the case where dispatch pushed commits (includes those if detected)
/// 4. Returns the extraction path for use by the agent
///
/// # Arguments
///
/// * `workspace` - Path to the workspace directory
/// * `config` - Extraction configuration
///
/// # Returns
///
/// Returns `ExtractionResult` containing the extraction path and metadata.
///
/// # Examples
///
/// ```no_run
/// use crate::dispatch::extraction::{extract_clean_workspace, ExtractionConfig};
///
/// let config = ExtractionConfig::new("worker-01".to_string(), "needle-abc123".to_string());
/// let result = extract_clean_workspace(&workspace, config).await?;
/// println!("Extracted to: {}", result.extraction_path.display());
/// ```
pub async fn extract_clean_workspace(
    workspace: &Path,
    config: ExtractionConfig,
) -> Result<ExtractionResult> {
    // Create per-dispatch temp directory under scratch
    let extraction_dir = create_extraction_directory(&config)?;

    tracing::info!(
        workspace = %workspace.display(),
        extraction_dir = %extraction_dir.display(),
        worker_id = %config.worker_id,
        bead_id = %config.bead_id,
        "creating clean workspace extraction"
    );

    // Determine the commit ref to extract
    // If pre_dispatch_head is set and HEAD moved, dispatch pushed commits
    // We'll extract HEAD which includes those commits
    let current_head = get_head_sha(workspace)?;
    let included_dispatch_commits = if let Some(ref pre_head) = config.pre_dispatch_head {
        current_head != *pre_head
    } else {
        false
    };

    if included_dispatch_commits {
        tracing::info!(
            pre_dispatch_head = %config.pre_dispatch_head.as_ref().unwrap(),
            current_head = %current_head,
            "dispatch pushed commits - extracting HEAD which includes new commits"
        );
    }

    // Extract git archive to the temp directory
    extract_git_archive_revision(workspace, &current_head, &extraction_dir)?;

    tracing::debug!(
        extraction_dir = %extraction_dir.display(),
        head_sha = %current_head,
        "successfully extracted clean workspace"
    );

    Ok(ExtractionResult {
        extraction_path: extraction_dir,
        included_dispatch_commits,
        extracted_head_sha: current_head,
    })
}

/// Create a per-dispatch temp directory under scratch.
///
/// Directory pattern: `$HOME/scratch/needle-<worker>-<bead_id>-<timestamp>/`
fn create_extraction_directory(config: &ExtractionConfig) -> Result<PathBuf> {
    let scratch_base = config
        .scratch_base
        .clone()
        .unwrap_or_else(default_scratch_base);

    // Ensure scratch base exists
    if !scratch_base.exists() {
        std::fs::create_dir_all(&scratch_base).with_context(|| {
            format!(
                "failed to create scratch base directory: {}",
                scratch_base.display()
            )
        })?;
    }

    // Create per-dispatch directory with timestamp for uniqueness
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let sanitized_bead_id = sanitize_filename(&config.bead_id);
    let dir_name = format!(
        "needle-{}-{}-{}",
        sanitize_filename(&config.worker_id),
        sanitized_bead_id,
        timestamp
    );

    let extraction_dir = scratch_base.join(dir_name);

    std::fs::create_dir_all(&extraction_dir).with_context(|| {
        format!(
            "failed to create extraction directory: {}",
            extraction_dir.display()
        )
    })?;

    Ok(extraction_dir)
}

/// Get the current HEAD SHA of a git repository.
fn get_head_sha(workspace: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace)
        .output()
        .context("failed to run git rev-parse HEAD")?;

    if !output.status.success() {
        anyhow::bail!(
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let sha = String::from_utf8(output.stdout)
        .context("git HEAD SHA is not valid UTF-8")?
        .trim()
        .to_string();

    Ok(sha)
}

/// Stream a committed tree to tar without holding the archive in NEEDLE's
/// address space. Both stderr pipes are drained concurrently and capped in
/// memory, so a noisy child cannot block the archive pipe.
pub(crate) fn extract_git_archive_revision(
    workspace: &Path,
    revision: &str,
    target_dir: &Path,
) -> Result<()> {
    extract_git_archive_with_commands(workspace, revision, target_dir, "git", "tar")
}

fn extract_git_archive_with_commands(
    workspace: &Path,
    revision: &str,
    target_dir: &Path,
    git_command: &str,
    tar_command: &str,
) -> Result<()> {
    let mut archive = Command::new(git_command)
        .args(["archive", "--format=tar", revision])
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to run git archive")?;
    let archive_stdout = archive.stdout.take().expect("git stdout was piped");
    let archive_stderr = archive.stderr.take().expect("git stderr was piped");
    let git_diagnostics = std::thread::spawn(move || drain_diagnostics(archive_stderr));

    let mut tar = match Command::new(tar_command)
        .args(["-x", "-f", "-"])
        .current_dir(target_dir)
        .stdin(Stdio::from(archive_stdout))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = archive.kill();
            let _ = archive.wait();
            let _ = git_diagnostics.join();
            return Err(error).context("failed to spawn tar extraction");
        }
    };
    let tar_stderr = tar.stderr.take().expect("tar stderr was piped");
    let tar_diagnostics = std::thread::spawn(move || drain_diagnostics(tar_stderr));

    let tar_status = tar.wait();
    if tar_status.is_err() {
        let _ = tar.kill();
        let _ = archive.kill();
        let _ = tar.wait();
    }
    let archive_status = archive.wait();
    let tar_error = tar_diagnostics.join().unwrap_or_default();
    let git_error = git_diagnostics.join().unwrap_or_default();
    let tar_status = tar_status.context("failed to wait for tar extraction")?;
    let archive_status = archive_status.context("failed to wait for git archive")?;
    if !tar_status.success() {
        anyhow::bail!(
            "tar extraction failed: {}",
            String::from_utf8_lossy(&tar_error).trim()
        );
    }
    if !archive_status.success() {
        anyhow::bail!(
            "git archive failed: {}",
            String::from_utf8_lossy(&git_error).trim()
        );
    }
    Ok(())
}

fn drain_diagnostics(mut stderr: impl std::io::Read) -> Vec<u8> {
    const MAX_DIAGNOSTICS: usize = 4096;
    let mut prefix = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let keep = count.min(MAX_DIAGNOSTICS - prefix.len());
                prefix.extend_from_slice(&chunk[..keep]);
            }
        }
    }
    prefix
}

/// Get the default scratch base directory (`$HOME/scratch`).
fn default_scratch_base() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join("scratch")
}

/// Sanitize a filename by replacing non-alphanumeric characters (except hyphen/underscore) with underscore.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Clean up an extraction directory.
///
/// This should be called on success to remove the temporary extraction.
/// On failure, the directory should be preserved for diagnosis.
pub async fn cleanup_extraction(extraction_dir: &Path) -> Result<()> {
    if extraction_dir.exists() {
        tracing::debug!(
            extraction_dir = %extraction_dir.display(),
            "cleaning up extraction directory"
        );
        tokio::fs::remove_dir_all(extraction_dir)
            .await
            .context("failed to remove extraction directory")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn committed_workspace() -> tempfile::TempDir {
        let workspace = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(workspace.path())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        std::fs::write(workspace.path().join("committed.txt"), "committed\n").unwrap();
        run(&["add", "committed.txt"]);
        run(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ]);
        workspace
    }

    #[test]
    fn archive_stream_extracts_only_the_captured_revision() {
        let workspace = committed_workspace();
        let revision = get_head_sha(workspace.path()).unwrap();
        std::fs::write(workspace.path().join("committed.txt"), "uncommitted\n").unwrap();
        std::fs::write(workspace.path().join("untracked.txt"), "untracked\n").unwrap();
        let target = tempfile::tempdir().unwrap();

        extract_git_archive_revision(workspace.path(), &revision, target.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(target.path().join("committed.txt")).unwrap(),
            "committed\n"
        );
        assert!(!target.path().join("untracked.txt").exists());
        assert!(!target.path().join(".git").exists());
    }

    #[cfg(unix)]
    #[test]
    fn noisy_tar_failure_does_not_block_archive_and_caps_diagnostics() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = committed_workspace();
        let target = tempfile::tempdir().unwrap();
        let fake_tar = target.path().join("noisy-tar.sh");
        std::fs::write(
            &fake_tar,
            "#!/bin/sh\nhead -c 131072 /dev/zero >&2\nexit 17\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_tar, std::fs::Permissions::from_mode(0o755)).unwrap();

        let start = std::time::Instant::now();
        let error = extract_git_archive_with_commands(
            workspace.path(),
            "HEAD",
            target.path(),
            "git",
            fake_tar.to_str().unwrap(),
        )
        .unwrap_err()
        .to_string();

        assert!(error.starts_with("tar extraction failed:"), "{error}");
        assert!(error.len() <= 4200, "diagnostics were unbounded");
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("valid-name_123"), "valid-name_123");
        assert_eq!(sanitize_filename("invalid name!@#"), "invalid_name___");
        assert_eq!(sanitize_filename("needle-abc123"), "needle-abc123");
    }

    #[test]
    fn test_default_scratch_base() {
        let base = default_scratch_base();
        assert!(base.ends_with("scratch"));
    }

    #[tokio::test]
    async fn test_create_extraction_directory() {
        let temp_base = tempfile::tempdir().unwrap();
        let config = ExtractionConfig {
            worker_id: "test-worker".to_string(),
            bead_id: "test-bead".to_string(),
            pre_dispatch_head: None,
            scratch_base: Some(temp_base.path().to_path_buf()),
        };

        let dir = create_extraction_directory(&config).unwrap();
        assert!(dir.exists());
        assert!(dir.is_dir());

        // Verify directory name pattern
        let dir_name = dir.file_name().unwrap().to_string_lossy();
        // create_extraction_directory joins every part with '-'.
        assert!(
            dir_name.starts_with("needle-test-worker-test-bead-"),
            "unexpected extraction directory name: {dir_name}"
        );
    }
}
