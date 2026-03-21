//! Hot-reload protocol for seamless binary upgrades.
//!
//! Workers detect new :stable binaries between bead cycles and re-exec
//! seamlessly. The `--resume` flag picks up state from the heartbeat file
//! and registry.
//!
//! ## Protocol
//!
//! 1. Between LOGGING and SELECTING: compare current binary hash to :stable hash
//! 2. If different: emit `upgrade.detected` telemetry, complete current cycle, re-exec
//! 3. `--resume`: pick up state from heartbeat file + registry, continue from SELECTING
//!
//! ## Rollback
//!
//! `needle rollback` restores `needle-stable.prev` as `needle-stable`.
//! Workers hot-reload on the next cycle after rollback.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::health::HeartbeatData;
use crate::registry::Registry;

// ──────────────────────────────────────────────────────────────────────────────
// BinaryHash
// ──────────────────────────────────────────────────────────────────────────────

/// A SHA-256 hash of a binary file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryHash([u8; 32]);

impl BinaryHash {
    /// Compute the SHA-256 hash of a binary file.
    pub fn compute(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("failed to read binary: {}", path.display()))?;
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let hash = hasher.finalize();
        Ok(BinaryHash(hash.into()))
    }

    /// Compute the hash of the currently running binary.
    pub fn current_exe() -> Result<Self> {
        let exe_path = std::env::current_exe()
            .context("failed to get current executable path")?;
        Self::compute(&exe_path)
    }

    /// Return the hash as a hex string.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// HotReloadChecker
// ──────────────────────────────────────────────────────────────────────────────

/// Checks for binary upgrades and performs hot-reload.
pub struct HotReloadChecker {
    needle_home: PathBuf,
    worker_id: String,
}

impl HotReloadChecker {
    /// Create a new hot-reload checker.
    pub fn new(config: &Config, worker_id: &str) -> Self {
        HotReloadChecker {
            needle_home: config.workspace.home.clone(),
            worker_id: worker_id.to_string(),
        }
    }

    /// Path to the stable binary.
    pub fn stable_binary(&self) -> PathBuf {
        self.needle_home.join("bin/needle-stable")
    }

    /// Path to the current executable.
    pub fn current_binary(&self) -> Result<PathBuf> {
        std::env::current_exe().context("failed to get current executable")
    }

    /// Check if the current binary is different from :stable.
    ///
    /// Returns `Some(stable_hash)` if an upgrade is detected, `None` if same.
    pub fn check_for_upgrade(&self) -> Result<Option<BinaryHash>> {
        let stable_path = self.stable_binary();

        // If :stable doesn't exist, no upgrade possible.
        if !stable_path.exists() {
            tracing::debug!("no :stable binary found, skipping upgrade check");
            return Ok(None);
        }

        let current_path = self.current_binary()?;
        let current_hash = BinaryHash::compute(&current_path)?;
        let stable_hash = BinaryHash::compute(&stable_path)?;

        if current_hash != stable_hash {
            tracing::info!(
                current = %current_path.display(),
                stable = %stable_path.display(),
                "upgrade detected: binary hash differs"
            );
            return Ok(Some(stable_hash));
        }

        Ok(None)
    }

    /// Perform hot-reload: re-exec the :stable binary with --resume.
    ///
    /// This function does not return on success.
    pub fn hot_reload(&self, resume_args: ResumeArgs) -> Result<()> {
        let stable_path = self.stable_binary();

        if !stable_path.exists() {
            bail!("cannot hot-reload: :stable binary not found at {}", stable_path.display());
        }

        // Build the command line with --resume.
        let mut args = vec![
            "run".to_string(),
            "--resume".to_string(),
            "--workspace".to_string(),
            resume_args.workspace.display().to_string(),
            "--identifier".to_string(),
            resume_args.worker_id.clone(),
        ];

        if let Some(ref agent) = resume_args.agent {
            args.push("--agent".to_string());
            args.push(agent.clone());
        }

        if let Some(timeout) = resume_args.timeout {
            args.push("--timeout".to_string());
            args.push(timeout.to_string());
        }

        tracing::info!(
            binary = %stable_path.display(),
            args = ?args,
            "re-executing with --resume for hot-reload"
        );

        // Re-exec: this replaces the current process.
        let err = exec::execvp(&stable_path, &args);
        bail!("failed to re-exec: {:?}", err);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ResumeArgs
// ──────────────────────────────────────────────────────────────────────────────

/// Arguments needed to resume a worker session.
#[derive(Debug, Clone)]
pub struct ResumeArgs {
    pub worker_id: String,
    pub workspace: PathBuf,
    pub agent: Option<String>,
    pub timeout: Option<u64>,
}

// ──────────────────────────────────────────────────────────────────────────────
// ResumeState
// ──────────────────────────────────────────────────────────────────────────────

/// State loaded from heartbeat and registry for resumption.
#[derive(Debug, Clone)]
pub struct ResumeState {
    pub worker_id: String,
    pub workspace: PathBuf,
    pub agent: String,
    pub beads_processed: u64,
    pub session: String,
}

impl ResumeState {
    /// Load resume state from heartbeat file and registry.
    ///
    /// Returns `None` if no valid heartbeat/registry entry exists.
    pub fn load(config: &Config, worker_id: &str) -> Result<Option<Self>> {
        let heartbeat_dir = config.workspace.home.join("state").join("heartbeats");
        let heartbeat_path = heartbeat_dir.join(format!("{}.json", worker_id));

        // Read heartbeat file.
        if !heartbeat_path.exists() {
            tracing::debug!(
                path = %heartbeat_path.display(),
                "no heartbeat file for resume"
            );
            return Ok(None);
        }

        let heartbeat_content = std::fs::read_to_string(&heartbeat_path)
            .with_context(|| format!("failed to read heartbeat: {}", heartbeat_path.display()))?;
        let heartbeat: HeartbeatData = serde_json::from_str(&heartbeat_content)
            .with_context(|| format!("failed to parse heartbeat: {}", heartbeat_path.display()))?;

        // Read registry entry.
        let registry = Registry::default_location(&config.workspace.home);
        let workers = registry.list().context("failed to list registry")?;
        let entry = workers.iter().find(|w| w.id == worker_id);

        let (agent, beads_processed) = match entry {
            Some(e) => (e.agent.clone(), e.beads_processed),
            None => {
                tracing::warn!("no registry entry for worker {}, using defaults", worker_id);
                (config.agent.default.clone(), heartbeat.beads_processed)
            }
        };

        Ok(Some(ResumeState {
            worker_id: heartbeat.worker_id,
            workspace: heartbeat.workspace,
            agent,
            beads_processed,
            session: heartbeat.session,
        }))
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Simple hex encoding without external crate (for BinaryHash display).
mod hex {
    const HEX_CHARS: &[u8] = b"0123456789abcdef";

    pub fn encode(bytes: [u8; 32]) -> String {
        let mut s = String::with_capacity(64);
        for b in &bytes {
            s.push(HEX_CHARS[(b >> 4) as usize] as char);
            s.push(HEX_CHARS[(b & 0x0f) as usize] as char);
        }
        s
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// exec helper
// ──────────────────────────────────────────────────────────────────────────────

mod exec {
    use std::path::Path;

    /// Execute a new program, replacing the current process.
    ///
    /// Uses `std::os::unix::process::CommandExt::exec()` which calls execvp(3).
    /// Returns an error if exec fails; on success this function does not return.
    #[cfg(unix)]
    pub fn execvp(path: &Path, args: &[String]) -> std::io::Error {
        use std::os::unix::process::CommandExt;
        std::process::Command::new(path).args(args).exec()
    }

    #[cfg(not(unix))]
    pub fn execvp(_path: &Path, _args: &[String]) -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "execvp not supported on this platform",
        )
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_hash_current_exe() {
        let hash = BinaryHash::current_exe().expect("should hash current exe");
        let hex = hash.to_hex();
        assert_eq!(hex.len(), 64, "SHA-256 hex should be 64 chars");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn binary_hash_deterministic() {
        let hash1 = BinaryHash::current_exe().expect("should hash current exe");
        let hash2 = BinaryHash::current_exe().expect("should hash current exe again");
        assert_eq!(hash1, hash2, "hashes should be equal for same file");
    }

    #[test]
    fn hex_encoding() {
        let bytes = [0u8; 32];
        let hex = hex::encode(bytes);
        assert_eq!(hex, "0".repeat(64));
    }

    #[test]
    fn hex_encoding_all_bytes() {
        let bytes: [u8; 32] = (0..=255u8).cycle().take(32).collect::<Vec<_>>().try_into().unwrap();
        let hex = hex::encode(bytes);
        assert_eq!(hex.len(), 64);
        // First byte 0x00 -> "00"
        assert_eq!(&hex[0..2], "00");
        // Second byte 0x01 -> "01"
        assert_eq!(&hex[2..4], "01");
    }
}
