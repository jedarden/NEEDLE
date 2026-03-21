//! Pulse strand: codebase health scans.
//!
//! Runs configured scanners (linters, test coverage, etc.) and creates beads
//! for significant findings that exceed severity thresholds.
//!
//! **Guardrails:**
//! - Opt-in only (disabled by default).
//! - Max beads created per run.
//! - 48-hour cooldown between runs per workspace.
//! - Severity threshold filters minor issues.
//! - Deduplication: issues already tracked don't create duplicate beads.
//!
//! **State:**
//! - `~/.needle/state/pulse/<workspace-hash>.json` stores:
//!   - Last run timestamp
//!   - Set of seen issue fingerprints
//!
//! Depends on: `bead_store`, `config`, `telemetry`, `types`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bead_store::BeadStore;
use crate::config::PulseConfig;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::StrandResult;

// ─── Scanner finding (parsed from scanner output) ─────────────────────────────

/// A single finding from a scanner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannerFinding {
    /// Human-readable title for the finding.
    pub title: String,
    /// Detailed description of the issue.
    pub body: String,
    /// Severity level (1=critical, 5=minor).
    pub severity: u8,
    /// File path if applicable.
    #[serde(default)]
    pub file_path: Option<String>,
    /// Line number if applicable.
    #[serde(default)]
    pub line: Option<u32>,
    /// Unique fingerprint for deduplication.
    pub fingerprint: String,
}

// ─── Persistent state ───────────────────────────────────────────────────────

/// Persisted state for the Pulse strand.
///
/// Tracks last run time and seen issue fingerprints for deduplication.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PulseState {
    /// Last time a pulse scan was run for this workspace.
    pub last_run: Option<DateTime<Utc>>,
    /// Set of issue fingerprints already seen (for dedup).
    pub seen_fingerprints: HashSet<String>,
}

impl PulseState {
    /// Load state from disk, returning default if file doesn't exist.
    fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data)
                .with_context(|| format!("failed to parse pulse state: {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => {
                Err(e).with_context(|| format!("failed to read pulse state: {}", path.display()))
            }
        }
    }

    /// Persist state to disk.
    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create state dir: {}", parent.display()))?;
        }
        let data = serde_json::to_string_pretty(self).context("failed to serialize pulse state")?;
        std::fs::write(path, data)
            .with_context(|| format!("failed to write pulse state: {}", path.display()))
    }

    /// Check if cooldown has elapsed since last run.
    fn is_in_cooldown(&self, cooldown_hours: i64) -> bool {
        match self.last_run {
            None => false,
            Some(last) => {
                let elapsed = Utc::now().signed_duration_since(last);
                elapsed.num_hours() < cooldown_hours
            }
        }
    }

    /// Check if a finding has already been seen.
    fn has_seen(&self, fingerprint: &str) -> bool {
        self.seen_fingerprints.contains(fingerprint)
    }

    /// Mark a finding as seen.
    fn mark_seen(&mut self, fingerprint: &str) {
        self.seen_fingerprints.insert(fingerprint.to_string());
    }

    /// Update the last run timestamp.
    fn touch(&mut self) {
        self.last_run = Some(Utc::now());
    }
}

// ─── PulseStrand ────────────────────────────────────────────────────────────

/// The Pulse strand — runs health scanners and creates beads for findings.
pub struct PulseStrand {
    config: PulseConfig,
    workspace: PathBuf,
    state_dir: PathBuf,
    telemetry: Telemetry,
}

impl PulseStrand {
    /// Create a new PulseStrand.
    ///
    /// `state_dir` is the base directory for pulse state files
    /// (e.g., `~/.needle/state/pulse/`).
    pub fn new(
        config: PulseConfig,
        workspace: PathBuf,
        state_dir: PathBuf,
        telemetry: Telemetry,
    ) -> Self {
        PulseStrand {
            config,
            workspace,
            state_dir,
            telemetry,
        }
    }

    /// Compute the state file path for a workspace.
    fn state_file_path(&self) -> PathBuf {
        let hash = workspace_hash(&self.workspace);
        self.state_dir.join(format!("{hash}.json"))
    }

    /// Run a scanner command and capture its output.
    async fn run_scanner(&self, name: &str, command: &str) -> Result<String> {
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.workspace)
            .output()
            .await
            .with_context(|| format!("failed to run scanner '{}'", name))?;

        // Combine stdout and stderr for analysis
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

        if !stderr.is_empty() {
            Ok(format!("{}\n{}", stdout, stderr))
        } else {
            Ok(stdout)
        }
    }

    /// Parse scanner output into findings.
    ///
    /// This is a simple heuristic parser. For production use, each scanner
    /// should have a dedicated parser for its output format.
    fn parse_output(&self, scanner_name: &str, output: &str) -> Vec<ScannerFinding> {
        let mut findings = Vec::new();
        let severity_threshold = self.config.severity_threshold;

        for line in output.lines() {
            // Skip empty lines and summary lines
            if line.trim().is_empty() || line.contains("warning:") && line.contains("generated") {
                continue;
            }

            // Heuristic: look for common patterns
            let is_warning = line.contains("warning:")
                || line.contains("WARN")
                || line.to_lowercase().contains("warning");
            let is_error = line.contains("error:")
                || line.contains("ERROR")
                || line.to_lowercase().contains("error:");

            if !is_warning && !is_error {
                continue;
            }

            // Determine severity (errors are higher severity)
            let severity = if is_error { 2 } else { 4 };
            if severity > severity_threshold {
                continue;
            }

            // Extract file path if present (common patterns)
            let file_path = extract_file_path(line);

            // Generate fingerprint
            let fingerprint = generate_fingerprint(scanner_name, line);

            // Create title from the line
            let title = if line.len() > 100 {
                format!("[{}] {}...", scanner_name, &line[..97])
            } else {
                format!("[{}] {}", scanner_name, line.trim())
            };

            findings.push(ScannerFinding {
                title,
                body: line.to_string(),
                severity,
                file_path,
                line: None,
                fingerprint,
            });
        }

        findings
    }

    /// Build the pulse prompt for agent-assisted analysis.
    #[cfg(test)]
    fn build_prompt(&self, scanner_name: &str, output: &str) -> String {
        if let Some(template) = &self.config.prompt_template {
            return template
                .replace("{scanner}", scanner_name)
                .replace("{output}", output)
                .replace("{workspace}", &self.workspace.display().to_string());
        }

        format!(
            "## Scanner: {}\n\n\
             **Workspace:** {}\n\n\
             **Output:**\n```\n{}\n```\n\n\
             ## Task\n\n\
             Analyze the scanner output above. Identify the most significant issues \
             that require attention. For each issue, provide:\n\
             - **title**: concise description (max 80 chars)\n\
             - **body**: detailed explanation with file/location if available\n\
             - **severity**: 1 (critical) to 5 (minor)\n\n\
             Output a JSON array of objects. If no significant issues, respond with: NO_ISSUES",
            scanner_name,
            self.workspace.display(),
            output
        )
    }
}

#[async_trait::async_trait]
impl super::Strand for PulseStrand {
    fn name(&self) -> &str {
        "pulse"
    }

    async fn evaluate(&self, store: &dyn BeadStore) -> StrandResult {
        // Guard: disabled.
        if !self.config.enabled {
            tracing::debug!("pulse strand disabled");
            return StrandResult::NoWork;
        }

        // Guard: no scanners configured.
        if self.config.scanners.is_empty() {
            tracing::debug!("pulse strand: no scanners configured");
            return StrandResult::NoWork;
        }

        // Load persistent state.
        let state_path = self.state_file_path();
        let mut state = match PulseState::load(&state_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to load pulse state, using defaults");
                PulseState::default()
            }
        };

        // Guard: cooldown.
        if state.is_in_cooldown(self.config.cooldown_hours as i64) {
            tracing::debug!(
                cooldown_hours = self.config.cooldown_hours,
                "pulse strand: in cooldown, skipping"
            );
            self.telemetry
                .emit(EventKind::PulseSkipped {
                    reason: "cooldown".to_string(),
                })
                .ok();
            return StrandResult::NoWork;
        }

        // Run scanners and collect findings.
        let mut all_findings: Vec<(String, ScannerFinding)> = Vec::new();

        for scanner in &self.config.scanners {
            tracing::info!(
                scanner = %scanner.name,
                command = %scanner.command,
                "pulse strand: running scanner"
            );

            self.telemetry
                .emit(EventKind::PulseScannerStarted {
                    scanner_name: scanner.name.clone(),
                })
                .ok();

            let output = match self.run_scanner(&scanner.name, &scanner.command).await {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(
                        scanner = %scanner.name,
                        error = %e,
                        "pulse strand: scanner failed"
                    );
                    self.telemetry
                        .emit(EventKind::PulseScannerFailed {
                            scanner_name: scanner.name.clone(),
                            error: e.to_string(),
                        })
                        .ok();
                    continue;
                }
            };

            // Parse findings
            let findings = self.parse_output(&scanner.name, &output);

            // Filter by scanner-specific severity threshold if set
            let threshold = scanner
                .severity_threshold
                .unwrap_or(self.config.severity_threshold);

            for finding in &findings {
                if finding.severity <= threshold && !state.has_seen(&finding.fingerprint) {
                    all_findings.push((scanner.name.clone(), finding.clone()));
                }
            }

            // Emit telemetry for this scanner
            self.telemetry
                .emit(EventKind::PulseScannerCompleted {
                    scanner_name: scanner.name.clone(),
                    findings_count: findings.len() as u32,
                })
                .ok();
        }

        // Update state timestamp
        state.touch();

        if all_findings.is_empty() {
            tracing::info!("pulse strand: no new significant findings");
            if let Err(e) = state.save(&state_path) {
                tracing::warn!(error = %e, "pulse strand: failed to save state");
            }
            return StrandResult::NoWork;
        }

        // Sort by severity (lower = more severe)
        all_findings.sort_by_key(|(_, f)| f.severity);

        // Create beads up to max_beads_per_run
        let mut created = 0u32;
        for (scanner_name, finding) in all_findings {
            if created >= self.config.max_beads_per_run {
                break;
            }

            let bead_title = format!("[Pulse] {}", finding.title);
            let bead_body = format!(
                "## Scanner Finding\n\n\
                 **Scanner:** {}\n\
                 **Severity:** {}/5 (1=critical)\n\
                 **File:** {}\n\n\
                 **Description:**\n{}\n\n\
                 ---\n\
                 This issue was detected by the pulse strand during a codebase health scan.",
                scanner_name,
                finding.severity,
                finding.file_path.as_deref().unwrap_or("(unknown)"),
                finding.body,
            );
            let labels: Vec<&str> = vec!["pulse-finding"];

            match store.create_bead(&bead_title, &bead_body, &labels).await {
                Ok(bead_id) => {
                    tracing::info!(
                        bead_id = %bead_id,
                        scanner = %scanner_name,
                        severity = finding.severity,
                        "pulse strand: created bead for finding"
                    );
                    self.telemetry
                        .emit(EventKind::PulseBeadCreated {
                            bead_id: bead_id.clone(),
                            scanner_name: scanner_name.clone(),
                            severity: finding.severity,
                        })
                        .ok();
                    state.mark_seen(&finding.fingerprint);
                    created += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        scanner = %scanner_name,
                        error = %e,
                        "pulse strand: failed to create bead for finding"
                    );
                }
            }
        }

        // Save state.
        if let Err(e) = state.save(&state_path) {
            tracing::warn!(error = %e, "pulse strand: failed to save state");
        }

        if created > 0 {
            tracing::info!(
                created,
                "pulse strand: created beads for codebase health findings"
            );
            StrandResult::WorkCreated
        } else {
            tracing::info!("pulse strand: no new beads created (all findings deduplicated)");
            StrandResult::NoWork
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Compute a short SHA-256 hash of a workspace path (for state filenames).
fn workspace_hash(workspace: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workspace.display().to_string().as_bytes());
    let result = hasher.finalize();
    result
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

/// Extract file path from a scanner output line (best effort).
fn extract_file_path(line: &str) -> Option<String> {
    // Common patterns:
    // - "src/main.rs:42:5: error: ..."
    // - "error[src/E001]: ..."
    // - "./lib/foo.js line 42: ..."

    // Pattern: path:line:col
    for part in line.split_whitespace().take(3) {
        if part.contains(':')
            && (part.contains('/')
                || part.contains(".rs")
                || part.contains(".js")
                || part.contains(".ts")
                || part.contains(".py"))
        {
            // Extract just the file path part (before the first colon)
            if let Some(path) = part.split(':').next() {
                if !path.is_empty() && path != "error" && path != "warning" {
                    return Some(path.to_string());
                }
            }
        }
    }
    None
}

/// Generate a unique fingerprint for a finding.
fn generate_fingerprint(scanner_name: &str, line: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scanner_name.as_bytes());
    hasher.update(line.trim().as_bytes());
    let result = hasher.finalize();
    result
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

// ─── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bead_store::{Filters, RepairReport};
    use crate::strand::Strand;
    use crate::types::{Bead, BeadId, ClaimResult};

    use chrono::Utc;
    use std::sync::Mutex;

    // ── Mock BeadStore ──────────────────────────────────────────────────

    struct MockStore {
        created: Mutex<Vec<(String, String, Vec<String>)>>,
    }

    impl MockStore {
        fn new() -> Self {
            MockStore {
                created: Mutex::new(Vec::new()),
            }
        }

        fn created_beads(&self) -> Vec<(String, String, Vec<String>)> {
            self.created.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl BeadStore for MockStore {
        async fn list_all(&self) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn ready(&self, _filters: &Filters) -> Result<Vec<Bead>> {
            Ok(vec![])
        }
        async fn show(&self, _id: &BeadId) -> Result<Bead> {
            anyhow::bail!("not implemented")
        }
        async fn claim(&self, _id: &BeadId, _actor: &str) -> Result<ClaimResult> {
            Ok(ClaimResult::NotClaimable {
                reason: "mock".to_string(),
            })
        }
        async fn release(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn reopen(&self, _id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
            Ok(())
        }
        async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
            self.created.lock().unwrap().push((
                title.to_string(),
                body.to_string(),
                labels.iter().map(|s| s.to_string()).collect(),
            ));
            let id = format!("pulse-{}", self.created.lock().unwrap().len());
            Ok(BeadId::from(id))
        }
        async fn add_dependency(&self, _blocker_id: &BeadId, _blocked_id: &BeadId) -> Result<()> {
            Ok(())
        }
        async fn doctor_repair(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn doctor_check(&self) -> Result<RepairReport> {
            Ok(RepairReport::default())
        }
        async fn full_rebuild(&self) -> Result<()> {
            Ok(())
        }
    }

    // ── State tests ─────────────────────────────────────────────────────

    #[test]
    fn state_load_missing_returns_default() {
        let path = PathBuf::from("/tmp/nonexistent-pulse-state-12345.json");
        let state = PulseState::load(&path).unwrap();
        assert!(state.last_run.is_none());
        assert!(state.seen_fingerprints.is_empty());
    }

    #[test]
    fn state_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let mut state = PulseState::default();
        state.touch();
        state.mark_seen("fingerprint-1");
        state.mark_seen("fingerprint-2");
        state.save(&path).unwrap();

        let loaded = PulseState::load(&path).unwrap();
        assert!(loaded.last_run.is_some());
        assert!(loaded.has_seen("fingerprint-1"));
        assert!(loaded.has_seen("fingerprint-2"));
        assert!(!loaded.has_seen("fingerprint-3"));
    }

    #[test]
    fn state_cooldown_not_in_cooldown_when_new() {
        let state = PulseState::default();
        assert!(!state.is_in_cooldown(48));
    }

    #[test]
    fn state_cooldown_in_cooldown_when_recent() {
        let mut state = PulseState::default();
        state.touch();
        assert!(state.is_in_cooldown(48));
    }

    #[test]
    fn state_cooldown_elapsed_after_period() {
        let state = PulseState {
            last_run: Some(Utc::now() - chrono::Duration::hours(50)),
            ..PulseState::default()
        };
        assert!(!state.is_in_cooldown(48));
    }

    // ── Strand tests ────────────────────────────────────────────────────

    #[tokio::test]
    async fn strand_name_is_pulse() {
        let telemetry = Telemetry::new("test".to_string());
        let strand = PulseStrand::new(
            PulseConfig::default(),
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/state"),
            telemetry,
        );
        assert_eq!(strand.name(), "pulse");
    }

    #[tokio::test]
    async fn disabled_returns_no_work() {
        let telemetry = Telemetry::new("test".to_string());
        let strand = PulseStrand::new(
            PulseConfig::default(), // disabled by default
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/state"),
            telemetry,
        );
        let store = MockStore::new();
        let result = strand.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn no_scanners_returns_no_work() {
        let telemetry = Telemetry::new("test".to_string());
        let config = PulseConfig {
            enabled: true,
            scanners: vec![],
            ..PulseConfig::default()
        };
        let strand = PulseStrand::new(
            config,
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/state"),
            telemetry,
        );
        let store = MockStore::new();
        let result = strand.evaluate(&store).await;
        assert!(matches!(result, StrandResult::NoWork));
    }

    #[tokio::test]
    async fn cooldown_skips_scan() {
        let state_dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());

        // Pre-populate state with recent run
        let mut state = PulseState::default();
        state.touch();
        let hash = workspace_hash(Path::new("/tmp/test"));
        let state_path = state_dir.path().join(format!("{hash}.json"));
        state.save(&state_path).unwrap();

        let config = PulseConfig {
            enabled: true,
            scanners: vec![crate::config::ScannerConfig {
                name: "test".to_string(),
                command: "echo 'warning: test'".to_string(),
                severity_threshold: None,
            }],
            cooldown_hours: 48,
            ..PulseConfig::default()
        };

        let strand = PulseStrand::new(
            config,
            PathBuf::from("/tmp/test"),
            state_dir.path().to_path_buf(),
            telemetry,
        );
        let store = MockStore::new();
        let result = strand.evaluate(&store).await;

        assert!(matches!(result, StrandResult::NoWork));
        assert!(store.created_beads().is_empty());
    }

    // ── Helper tests ─────────────────────────────────────────────────────

    #[test]
    fn workspace_hash_is_deterministic() {
        let h1 = workspace_hash(Path::new("/home/user/project"));
        let h2 = workspace_hash(Path::new("/home/user/project"));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn extract_file_path_finds_colon_pattern() {
        let line = "src/main.rs:42:5: error: unused variable";
        let path = extract_file_path(line);
        assert_eq!(path, Some("src/main.rs".to_string()));
    }

    #[test]
    fn extract_file_path_returns_none_for_no_path() {
        let line = "error: something went wrong";
        let path = extract_file_path(line);
        assert!(path.is_none());
    }

    #[test]
    fn generate_fingerprint_is_consistent() {
        let fp1 = generate_fingerprint("clippy", "src/main.rs:42: error: x");
        let fp2 = generate_fingerprint("clippy", "src/main.rs:42: error: x");
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 16);
    }

    #[test]
    fn generate_fingerprint_differs_for_different_input() {
        let fp1 = generate_fingerprint("clippy", "error A");
        let fp2 = generate_fingerprint("clippy", "error B");
        assert_ne!(fp1, fp2);
    }

    // ── Config tests ─────────────────────────────────────────────────────

    #[test]
    fn default_config_is_disabled() {
        let config = PulseConfig::default();
        assert!(!config.enabled);
        assert!(config.scanners.is_empty());
        assert_eq!(config.max_beads_per_run, 5);
        assert_eq!(config.cooldown_hours, 48);
        assert_eq!(config.severity_threshold, 3);
    }

    // ── Parse output tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn parse_output_extracts_warnings() {
        let telemetry = Telemetry::new("test".to_string());
        let strand = PulseStrand::new(
            PulseConfig {
                severity_threshold: 5, // Accept all severities
                ..PulseConfig::default()
            },
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/state"),
            telemetry,
        );

        let output = "src/main.rs:10:5: warning: unused variable\n\
                       src/lib.rs:20:1: error: mismatched types";

        let findings = strand.parse_output("test", output);

        assert_eq!(findings.len(), 2);
        // Findings are in line order (warning first, error second)
        assert!(findings[0].title.contains("warning"));
        assert!(findings[1].title.contains("error"));
    }

    #[tokio::test]
    async fn parse_output_respects_severity() {
        let telemetry = Telemetry::new("test".to_string());
        let strand = PulseStrand::new(
            PulseConfig {
                severity_threshold: 2, // Only errors (severity 1-2)
                ..PulseConfig::default()
            },
            PathBuf::from("/tmp"),
            PathBuf::from("/tmp/state"),
            telemetry,
        );

        let output = "warning: minor issue\nerror: critical issue";
        let findings = strand.parse_output("test", output);

        // Only the error should pass (severity 2 <= threshold 2)
        assert_eq!(findings.len(), 1);
        assert!(findings[0].title.contains("error"));
    }

    // ── End-to-end tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn scanner_runs_and_creates_beads() {
        let state_dir = tempfile::tempdir().unwrap();
        let workspace_dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());

        let config = PulseConfig {
            enabled: true,
            scanners: vec![crate::config::ScannerConfig {
                name: "echo-scanner".to_string(),
                command: "echo 'src/foo.rs:10:1: error: unused import'".to_string(),
                severity_threshold: None,
            }],
            cooldown_hours: 0,
            severity_threshold: 5, // Accept all
            max_beads_per_run: 10,
            ..PulseConfig::default()
        };

        let strand = PulseStrand::new(
            config,
            workspace_dir.path().to_path_buf(),
            state_dir.path().to_path_buf(),
            telemetry,
        );
        let store = MockStore::new();
        let result = strand.evaluate(&store).await;

        assert!(matches!(result, StrandResult::WorkCreated));
        let beads = store.created_beads();
        assert_eq!(beads.len(), 1);
        assert!(beads[0].0.contains("[Pulse]"));
        assert!(beads[0].0.contains("error"));
        assert!(beads[0].2.contains(&"pulse-finding".to_string()));
    }

    #[tokio::test]
    async fn dedup_prevents_duplicate_beads_across_scans() {
        let state_dir = tempfile::tempdir().unwrap();
        let workspace_dir = tempfile::tempdir().unwrap();

        let config = PulseConfig {
            enabled: true,
            scanners: vec![crate::config::ScannerConfig {
                name: "echo-scanner".to_string(),
                command: "echo 'error: same issue every time'".to_string(),
                severity_threshold: None,
            }],
            cooldown_hours: 0, // No cooldown so we can run twice
            severity_threshold: 5,
            max_beads_per_run: 10,
            ..PulseConfig::default()
        };

        let ws = workspace_dir.path().to_path_buf();

        // First scan: should create a bead
        let telemetry = Telemetry::new("test".to_string());
        let strand = PulseStrand::new(
            config.clone(),
            ws.clone(),
            state_dir.path().to_path_buf(),
            telemetry,
        );
        let store1 = MockStore::new();
        let result1 = strand.evaluate(&store1).await;
        assert!(matches!(result1, StrandResult::WorkCreated));
        assert_eq!(store1.created_beads().len(), 1);

        // Second scan: same output, should NOT create a bead (dedup)
        let telemetry2 = Telemetry::new("test".to_string());
        let strand2 = PulseStrand::new(config, ws, state_dir.path().to_path_buf(), telemetry2);
        let store2 = MockStore::new();
        let result2 = strand2.evaluate(&store2).await;
        assert!(matches!(result2, StrandResult::NoWork));
        assert!(store2.created_beads().is_empty());
    }

    #[tokio::test]
    async fn max_beads_per_run_limits_creation() {
        let state_dir = tempfile::tempdir().unwrap();
        let workspace_dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());

        // Scanner outputs 3 errors but max_beads_per_run is 2
        let config = PulseConfig {
            enabled: true,
            scanners: vec![crate::config::ScannerConfig {
                name: "multi-error".to_string(),
                command: "printf 'error: issue one\\nerror: issue two\\nerror: issue three\\n'"
                    .to_string(),
                severity_threshold: None,
            }],
            cooldown_hours: 0,
            severity_threshold: 5,
            max_beads_per_run: 2,
            ..PulseConfig::default()
        };

        let strand = PulseStrand::new(
            config,
            workspace_dir.path().to_path_buf(),
            state_dir.path().to_path_buf(),
            telemetry,
        );
        let store = MockStore::new();
        let result = strand.evaluate(&store).await;

        assert!(matches!(result, StrandResult::WorkCreated));
        assert_eq!(store.created_beads().len(), 2);
    }

    #[tokio::test]
    async fn scanner_with_no_findings_returns_no_work() {
        let state_dir = tempfile::tempdir().unwrap();
        let workspace_dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());

        let config = PulseConfig {
            enabled: true,
            scanners: vec![crate::config::ScannerConfig {
                name: "clean-scanner".to_string(),
                command: "echo 'all checks passed'".to_string(),
                severity_threshold: None,
            }],
            cooldown_hours: 0,
            severity_threshold: 5,
            max_beads_per_run: 10,
            ..PulseConfig::default()
        };

        let strand = PulseStrand::new(
            config,
            workspace_dir.path().to_path_buf(),
            state_dir.path().to_path_buf(),
            telemetry,
        );
        let store = MockStore::new();
        let result = strand.evaluate(&store).await;

        assert!(matches!(result, StrandResult::NoWork));
        assert!(store.created_beads().is_empty());
    }

    #[tokio::test]
    async fn state_persists_after_scan() {
        let state_dir = tempfile::tempdir().unwrap();
        let workspace_dir = tempfile::tempdir().unwrap();
        let telemetry = Telemetry::new("test".to_string());

        let config = PulseConfig {
            enabled: true,
            scanners: vec![crate::config::ScannerConfig {
                name: "persist-test".to_string(),
                command: "echo 'error: test finding'".to_string(),
                severity_threshold: None,
            }],
            cooldown_hours: 0,
            severity_threshold: 5,
            max_beads_per_run: 10,
            ..PulseConfig::default()
        };

        let strand = PulseStrand::new(
            config,
            workspace_dir.path().to_path_buf(),
            state_dir.path().to_path_buf(),
            telemetry,
        );
        let store = MockStore::new();
        strand.evaluate(&store).await;

        // Verify state was persisted
        let state_path = strand.state_file_path();
        let state = PulseState::load(&state_path).unwrap();
        assert!(state.last_run.is_some());
        assert!(!state.seen_fingerprints.is_empty());
    }

    #[tokio::test]
    async fn build_prompt_uses_custom_template() {
        let telemetry = Telemetry::new("test".to_string());
        let strand = PulseStrand::new(
            PulseConfig {
                prompt_template: Some(
                    "Scanner: {scanner}\nOutput: {output}\nWorkspace: {workspace}".to_string(),
                ),
                ..PulseConfig::default()
            },
            PathBuf::from("/my/workspace"),
            PathBuf::from("/tmp/state"),
            telemetry,
        );

        let prompt = strand.build_prompt("clippy", "error: test");
        assert!(prompt.contains("Scanner: clippy"));
        assert!(prompt.contains("Output: error: test"));
        assert!(prompt.contains("Workspace: /my/workspace"));
    }

    #[tokio::test]
    async fn build_prompt_uses_default_when_no_template() {
        let telemetry = Telemetry::new("test".to_string());
        let strand = PulseStrand::new(
            PulseConfig::default(),
            PathBuf::from("/my/workspace"),
            PathBuf::from("/tmp/state"),
            telemetry,
        );

        let prompt = strand.build_prompt("clippy", "error: test");
        assert!(prompt.contains("## Scanner: clippy"));
        assert!(prompt.contains("## Task"));
        assert!(prompt.contains("error: test"));
    }
}
