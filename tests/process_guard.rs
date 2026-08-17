//! Shared ProcessGuard test utility for child process cleanup.
//!
//! This module provides a reusable `ProcessGuard` struct that ensures proper
//! cleanup of child processes in integration tests, even if the test panics.
//!
//! # Example
//!
//! ```no_run
//! use tests::process_guard::ProcessGuard;
//! use std::process::Command;
//!
//! #[tokio::test]
//! async fn test_with_process() {
//!     let child = Command::new("sleep")
//!         .arg("10")
//!         .spawn()
//!         .expect("failed to spawn process");
//!
//!     let mut guard = ProcessGuard::new(child);
//!
//!     // Test logic here...
//!
//!     // ProcessGuard ensures cleanup on drop, even if test panics
//!     let _ = guard.wait();
//! }
//! ```

use std::io;
use std::process::{Child, ExitStatus};

/// Guard for a child process that ensures cleanup on drop.
///
/// `ProcessGuard` wraps a `std::process::Child` and provides:
/// - Automatic cleanup via `Drop` implementation (kills and waits to prevent zombies)
/// - Interface methods (`wait()`, `kill()`, `try_wait()`) for explicit control
/// - Safe ownership transfer to prevent double-wait issues
///
/// # Purpose
///
/// In integration tests that spawn long-running worker processes, we need to ensure:
/// 1. The process is cleaned up even if the test panics
/// 2. Zombies are prevented (killed processes must be waited on)
/// 3. Tests have explicit control over process lifecycle
///
/// # Implementation
///
/// The guard wraps `Option<Child>` to enable safe ownership transfer in `Drop`.
/// When dropped, it kills the process and waits for the exit status to prevent zombies.
#[derive(Debug)]
pub struct ProcessGuard {
    inner: Option<Child>,
}

impl ProcessGuard {
    /// Create a new ProcessGuard from a child process.
    ///
    /// # Arguments
    ///
    /// * `child` - The child process to guard
    ///
    /// # Returns
    ///
    /// Returns a `ProcessGuard` that takes ownership of the child process.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use std::process::Command;
    /// # use tests::process_guard::ProcessGuard;
    /// let child = Command::new("sleep").arg("10").spawn().unwrap();
    /// let guard = ProcessGuard::new(child);
    /// ```
    pub fn new(child: Child) -> Self {
        Self { inner: Some(child) }
    }

    /// Try to wait for the process without blocking.
    ///
    /// This method checks if the process has exited without blocking the thread.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(status))` - Process has exited with the given status
    /// - `Ok(None)` - Process is still running
    /// - `Err(e)` - Error occurred while checking process status
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use tests::process_guard::ProcessGuard;
    /// # let mut guard = ProcessGuard::new(unimplemented!());
    /// match guard.try_wait() {
    ///     Ok(Some(status)) => println!("Process exited: {:?}", status),
    ///     Ok(None) => println!("Process still running"),
    ///     Err(e) => eprintln!("Error checking process: {}", e),
    /// }
    /// ```
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(ref mut child) = self.inner {
            child.try_wait()
        } else {
            Ok(None)
        }
    }

    /// Kill the child process.
    ///
    /// Sends SIGTERM (or equivalent on non-Unix) to the child process.
    /// Does not wait for the process to exit - call `wait()` separately to prevent zombies.
    ///
    /// # Returns
    ///
    /// - `Ok(())` - Process was killed (or was already None)
    /// - `Err(e)` - Error occurred while killing the process
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use tests::process_guard::ProcessGuard;
    /// # let mut guard = ProcessGuard::new(unimplemented!());
    /// guard.kill().expect("failed to kill process");
    /// guard.wait().expect("failed to wait for killed process");
    /// ```
    pub fn kill(&mut self) -> io::Result<()> {
        if let Some(ref mut child) = self.inner {
            child.kill()
        } else {
            Ok(())
        }
    }

    /// Wait for the child process to exit.
    ///
    /// Blocks until the process exits and returns its exit status.
    ///
    /// # Returns
    ///
    /// - `Ok(status)` - Process exited with the given status
    /// - `Err(e)` - Error occurred while waiting, or no child process exists
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use tests::process_guard::ProcessGuard;
    /// # let mut guard = ProcessGuard::new(unimplemented!());
    /// let status = guard.wait().expect("failed to wait for process");
    /// println!("Process exited with status: {:?}", status);
    /// ```
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(ref mut child) = self.inner {
            child.wait()
        } else {
            Err(io::Error::other("No child process to wait for"))
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        // Best-effort cleanup - don't panic in drop
        let _ = self.kill();
        let _ = self.wait();
        // Prevent double-wait by consuming the child after our methods handle it
        let _ = self.inner.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_process_guard_cleanup_on_drop() {
        // Spawn a long-running process
        let child = std::process::Command::new("sleep")
            .arg("10")
            .spawn()
            .expect("failed to spawn sleep process");

        let pid = child.id();
        let guard = ProcessGuard::new(child);

        // Process should be running
        assert!(process_exists(pid), "process should be running");

        // When guard is dropped, process should be killed and cleaned up
        drop(guard);
        std::thread::sleep(Duration::from_millis(100));

        // Process should no longer exist
        assert!(
            !process_exists(pid),
            "process should be terminated after drop"
        );
    }

    #[test]
    fn test_process_guard_explicit_wait() {
        // Spawn a short-lived process
        let child = std::process::Command::new("true")
            .spawn()
            .expect("failed to spawn true process");

        let mut guard = ProcessGuard::new(child);

        // Wait for process to complete
        let status = guard.wait().expect("failed to wait for process");
        assert!(status.success(), "true should exit successfully");
    }

    #[test]
    fn test_process_guard_kill() {
        // Spawn a long-running process
        let child = std::process::Command::new("sleep")
            .arg("10")
            .spawn()
            .expect("failed to spawn sleep process");

        let pid = child.id();
        let mut guard = ProcessGuard::new(child);

        // Process should be running
        assert!(process_exists(pid), "process should be running");

        // Kill the process explicitly
        guard.kill().expect("failed to kill process");
        guard.wait().expect("failed to wait for killed process");

        // Process should no longer exist
        assert!(!process_exists(pid), "process should be terminated");
    }

    #[test]
    fn test_process_guard_try_wait() {
        // Spawn a long-running process
        let child = std::process::Command::new("sleep")
            .arg("1")
            .spawn()
            .expect("failed to spawn sleep process");

        let mut guard = ProcessGuard::new(child);

        // Process should still be running
        match guard.try_wait() {
            Ok(None) => {
                // Expected - process is still running
            }
            Ok(Some(status)) => {
                panic!(
                    "Process should still be running, but exited with: {:?}",
                    status
                );
            }
            Err(e) => {
                panic!("Failed to try_wait: {}", e);
            }
        }

        // Wait a bit and try again
        std::thread::sleep(Duration::from_millis(1100));
        match guard.try_wait() {
            Ok(Some(status)) => {
                assert!(status.success(), "sleep should exit successfully");
            }
            Ok(None) => {
                panic!("Process should have exited by now");
            }
            Err(e) => {
                panic!("Failed to try_wait: {}", e);
            }
        }
    }

    /// Helper to check if a process with given PID exists
    fn process_exists(pid: u32) -> bool {
        #[cfg(unix)]
        {
            use std::process::Command;
            Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }

        #[cfg(not(unix))]
        {
            // On non-Unix, we can't easily check process existence
            // Just return true to avoid breaking tests
            true
        }
    }
}
