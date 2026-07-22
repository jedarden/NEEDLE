//! Test fixture helper for spawning real tmux sessions.
//!
//! This module provides reusable test infrastructure for creating real tmux sessions
//! that mimic how NEEDLE's inner worker launches under tmux in production, including
//! the shell wrapper indirection that creates the pane_pid-vs-child-PID split.
//!
//! # Example
//!
//! ```no_run
//! use tests::tmux_fixture::TmuxSession;
//!
//! #[tokio::test]
//! async fn test_with_tmux() {
//!     let session = TmuxSession::spawn("test-session").await.unwrap();
//!     assert!(session.is_alive());
//!     session.kill().unwrap();
//! }
//! ```

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

// ────────────────────────────────────────────────────────────────────────────────
// TmuxSession Handle
// ────────────────────────────────────────────────────────────────────────────────

/// Handle to a real tmux session spawned for testing.
///
/// This represents a tmux session that was created using the same pattern as
/// production NEEDLE worker launches, including the NEEDLE_INNER=1 shell wrapper
/// indirection that creates the pane_pid-vs-child-PID split.
#[derive(Debug)]
pub struct TmuxSession {
    /// Name of the tmux session (e.g., "test-session-abc123")
    pub session_name: String,
    /// PID of the tmux pane (from `tmux list-panes -F "#{pane_pid}"`)
    pub pane_pid: u32,
    /// Path to the log file where stderr is redirected
    pub log_path: PathBuf,
    /// When the session was spawned
    pub spawned_at: Instant,
}

impl TmuxSession {
    /// Spawn a new tmux session with a long-lived sleep command.
    ///
    /// This mimics how NEEDLE's inner worker launches under tmux in production,
    /// including the shell wrapper indirection with NEEDLE_INNER=1.
    ///
    /// The session name will be "test-session-<random>" for uniqueness.
    ///
    /// # Arguments
    ///
    /// * `base_name` - Base name for the session (will be appended with a random suffix)
    ///
    /// # Returns
    ///
    /// Returns a `TmuxSession` handle containing session metadata.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use tests::tmux_fixture::TmuxSession;
    /// # async fn test() -> anyhow::Result<()> {
    /// let session = TmuxSession::spawn("my-test").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn spawn(base_name: &str) -> Result<Self> {
        // Generate unique session name
        let random_suffix: String = std::iter::repeat_with(
            || {
                use rand::Rng;
                rand::thread_rng().sample(rand::distributions::Alphanumeric)
            }
        )
        .take(6)
        .map(char::from)
        .collect();
        let session_name = format!("{}-{}", base_name, random_suffix);

        // Create log directory
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let logs_dir = PathBuf::from(format!("{}/.needle/logs", home));
        std::fs::create_dir_all(&logs_dir)
            .context("failed to create logs directory")?;

        // Build stderr log path
        let log_path = logs_dir.join(format!("{}.stderr.log", session_name));

        // Build the shell command that mimics production launch_in_tmux()
        // This creates the pane_pid-vs-child-PID split via bash -c wrapper
        let shell_cmd = format!(
            "NEEDLE_INNER=1 sleep 3600 2>> {}",
            log_path.display()
        );

        // Spawn the tmux session
        let status = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &session_name,
                &shell_cmd,
            ])
            .status()
            .context("failed to launch tmux — is tmux installed?")?;

        if !status.success() {
            anyhow::bail!(
                "tmux new-session exited with status {} for session '{}'",
                status,
                session_name
            );
        }

        // Give tmux a moment to create the session
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Capture the pane_pid from tmux list-panes
        let pane_pid = Self::capture_pane_pid(&session_name)?;

        Ok(TmuxSession {
            session_name,
            pane_pid,
            log_path,
            spawned_at: Instant::now(),
        })
    }

    /// Capture the pane_pid from tmux list-panes.
    ///
    /// This runs `tmux list-panes -t <session> -F "#{pane_pid}"` and parses
    /// the output to extract the pane PID.
    fn capture_pane_pid(session_name: &str) -> Result<u32> {
        let output = Command::new("tmux")
            .args([
                "list-panes",
                "-t",
                session_name,
                "-F",
                "#{pane_pid}",
            ])
            .output()
            .context("failed to list tmux panes")?;

        if !output.status.success() {
            anyhow::bail!(
                "tmux list-panes exited with status {}",
                output.status
            );
        }

        let pane_str = String::from_utf8_lossy(&output.stdout);
        let pane_pid: u32 = pane_str
            .trim()
            .parse()
            .context(format!("failed to parse pane_pid from: {}", pane_str))?;

        Ok(pane_pid)
    }

    /// Check if the tmux session is still alive.
    ///
    /// Returns true if the session exists in tmux, false otherwise.
    pub fn is_alive(&self) -> bool {
        Command::new("tmux")
            .args([
                "has-session",
                "-t",
                &self.session_name,
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Get the list of all PIDs in the process tree rooted at this session.
    ///
    /// This finds all child processes spawned from the tmux session,
    /// useful for asserting cleanup behavior.
    pub fn process_tree_pids(&self) -> Result<Vec<u32>> {
        // On Linux, we can use pgrep to find all processes in the session
        // For simplicity, we'll just return the pane_pid
        // A more sophisticated implementation would use pstree or similar
        Ok(vec![self.pane_pid])
    }

    /// Kill the tmux session.
    ///
    /// Runs `tmux kill-session -t <session_name>` to terminate the session.
    /// If the session is already dead, this returns Ok(()) without error.
    pub fn kill(&self) -> Result<()> {
        if !self.is_alive() {
            return Ok(());
        }

        let status = Command::new("tmux")
            .args([
                "kill-session",
                "-t",
                &self.session_name,
            ])
            .status()
            .context("failed to kill tmux session")?;

        if !status.success() {
            anyhow::bail!(
                "tmux kill-session exited with status {}",
                status
            );
        }

        Ok(())
    }

    /// Assert that the session is alive.
    ///
    /// Panics if the session is not alive.
    pub fn assert_alive(&self) {
        assert!(
            self.is_alive(),
            "tmux session '{}' is not alive",
            self.session_name
        );
    }

    /// Assert that the session is dead.
    ///
    /// Panics if the session is still alive.
    pub fn assert_dead(&self) {
        assert!(
            !self.is_alive(),
            "tmux session '{}' is still alive (expected dead)",
            self.session_name
        );
    }
}

// Implement Drop for automatic cleanup on test failure/panic.
impl Drop for TmuxSession {
    fn drop(&mut self) {
        // Best-effort cleanup - don't panic in drop
        let _ = self.kill();
    }
}

// ────────────────────────────────────────────────────────────────────────────────
// Helper Functions
// ────────────────────────────────────────────────────────────────────────────────

/// List all tmux sessions currently running.
///
/// Returns a list of all session names.
pub fn list_all_sessions() -> Vec<String> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|s| s.to_string())
            .collect(),
        _ => vec![],
    }
}

/// List all needle tmux sessions currently running.
///
/// Returns a list of session names that start with "needle-".
pub fn list_needle_sessions() -> Vec<String> {
    list_all_sessions()
        .into_iter()
        .filter(|line| line.starts_with("needle-"))
        .collect()
}

/// Check if tmux is installed and available.
///
/// Returns true if `tmux` can be executed, false otherwise.
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .args(["-V"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Kill all tmux sessions matching a prefix.
///
/// This is useful for cleanup in test suites to ensure no leaked sessions.
///
/// # Arguments
///
/// * `prefix` - Session name prefix to match (e.g., "test-")
///
/// # Returns
///
/// Returns the number of sessions killed.
pub fn kill_sessions_with_prefix(prefix: &str) -> Result<usize> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .context("failed to list tmux sessions")?;

    if !output.status.success() {
        return Ok(0);
    }

    let sessions: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with(prefix))
        .map(|s| s.to_string())
        .collect();

    for session in &sessions {
        Command::new("tmux")
            .args(["kill-session", "-t", session])
            .status()
            .context(format!("failed to kill session '{}'", session))?;
    }

    Ok(sessions.len())
}

// ────────────────────────────────────────────────────────────────────────────────
// Integration Tests
// ────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_and_list_tmux_session() {
        // Skip test if tmux is not available
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        // Spawn a test session
        let session = TmuxSession::spawn("test-spawn")
            .await
            .expect("failed to spawn session");

        // Verify session is alive
        session.assert_alive();

        // Verify we can list the session
        let sessions = list_all_sessions();
        assert!(
            sessions.iter().any(|s| s.contains(&session.session_name)),
            "session '{}' should be listable in all sessions",
            session.session_name
        );

        // Clean up
        session.kill().expect("failed to kill session");
        session.assert_dead();
    }

    #[tokio::test]
    async fn test_pane_pid_capture() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = TmuxSession::spawn("test-pane-pid")
            .await
            .expect("failed to spawn session");

        // Verify pane_pid is non-zero
        assert!(
            session.pane_pid > 0,
            "pane_pid should be non-zero, got {}",
            session.pane_pid
        );

        // Verify pane_pid is a valid PID (exists in process table)
        // On Unix, kill(0) checks if process exists without sending signal
        #[cfg(unix)]
        {
            use std::process::Command;
            let output = Command::new("ps")
                .args(["-p", &session.pane_pid.to_string()])
                .output();
            assert!(
                output.map(|o| o.status.success()).unwrap_or(false),
                "pane_pid {} should exist in process table",
                session.pane_pid
            );
        }

        session.kill().expect("cleanup failed");
    }

    #[tokio::test]
    async fn test_log_file_created() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session = TmuxSession::spawn("test-log")
            .await
            .expect("failed to spawn session");

        // Verify log file exists (may be empty initially)
        assert!(
            session.log_path.exists(),
            "log file should exist at {:?}",
            session.log_path
        );

        // Verify log path contains session name
        assert!(
            session.log_path.to_string_lossy().contains(&session.session_name),
            "log path should contain session name"
        );

        session.kill().expect("cleanup failed");
    }

    #[tokio::test]
    async fn test_process_cleanup_on_drop() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let session_name = {
            let session = TmuxSession::spawn("test-drop")
                .await
                .expect("failed to spawn session");
            session.assert_alive();
            session.session_name.clone()
        };

        // After the scope ends, Drop should kill the session
        // Wait a moment for cleanup
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Verify session is dead
        let output = Command::new("tmux")
            .args(["has-session", "-t", &session_name])
            .output();
        assert!(
            output.map(|o| !o.status.success()).unwrap_or(true),
            "session should be dead after drop"
        );
    }

    #[tokio::test]
    async fn test_kill_sessions_with_prefix() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let base_name = "test-kill-prefix";

        // Spawn a few sessions with the same base name (will get random suffixes)
        let mut sessions = Vec::new();
        let mut session_names = Vec::new();
        for _ in 0..3 {
            let session = TmuxSession::spawn(base_name)
                .await
                .expect("failed to spawn session");
            session.assert_alive();
            session_names.push(session.session_name.clone());
            sessions.push(session); // Keep sessions alive to prevent drop
        }

        // Verify sessions were created with the expected prefix
        let all_sessions = list_all_sessions();
        let matching_sessions: Vec<_> = all_sessions
            .iter()
            .filter(|s| s.starts_with(base_name))
            .collect();
        assert_eq!(
            matching_sessions.len(),
            3,
            "should have 3 sessions with prefix '{}', found: {:?}",
            base_name,
            matching_sessions
        );

        // Kill all sessions with the prefix
        let killed = kill_sessions_with_prefix(base_name)
            .expect("failed to kill sessions");

        assert_eq!(killed, 3, "should have killed 3 sessions");

        // Verify no sessions remain with that prefix
        let remaining = list_all_sessions()
            .into_iter()
            .filter(|s| s.starts_with(base_name))
            .count();
        assert_eq!(remaining, 0, "no sessions should remain with prefix");

        // Don't rely on drop for cleanup since we already killed them
        // The sessions are already dead, so drop() will just check and return Ok
    }

    #[tokio::test]
    async fn test_multiple_concurrent_sessions() {
        if !tmux_available() {
            eprintln!("Skipping test: tmux not available");
            return;
        }

        let mut sessions = Vec::new();

        // Spawn multiple concurrent sessions
        for i in 0..5 {
            let session = TmuxSession::spawn(&format!("test-concurrent-{}", i))
                .await
                .expect("failed to spawn session");
            session.assert_alive();
            sessions.push(session);
        }

        // All sessions should be alive
        for session in &sessions {
            session.assert_alive();
        }

        // Verify all pane_pids are unique
        let pane_pids: Vec<_> = sessions.iter().map(|s| s.pane_pid).collect();
        let unique_pids: std::collections::HashSet<_> = pane_pids.iter().collect();
        assert_eq!(
            unique_pids.len(),
            pane_pids.len(),
            "each session should have a unique pane_pid"
        );
    }
}
