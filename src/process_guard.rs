//! Process-group kill guard for spawned agent subprocesses.
//!
//! Depends on: nothing (leaf module — only `libc`).

use tokio::process::Child;

// ──────────────────────────────────────────────────────────────────────────────
// ProcessGuard
// ──────────────────────────────────────────────────────────────────────────────

/// Guard for a spawned child process that ensures cleanup on drop.
///
/// Wraps a `tokio::process::Child` and provides a `wait()` method. If dropped
/// before `wait()` is called, the child process is killed to prevent orphaning.
pub struct ProcessGuard {
    child: Option<Child>,
    pid: u32,
}

impl ProcessGuard {
    /// Create a new ProcessGuard from a spawned Child.
    ///
    /// # Arguments
    /// * `child` - The spawned child process to guard
    pub fn new(child: Child) -> Self {
        let pid = child.id().unwrap_or(0);
        Self {
            child: Some(child),
            pid,
        }
    }

    /// Get the process ID.
    pub fn id(&self) -> u32 {
        self.pid
    }

    /// Wait for the child to exit and return its exit status.
    ///
    /// This consumes the guard and returns the exit status. After calling this,
    /// the child is considered reaped and no cleanup will be performed on drop.
    pub async fn wait(mut self) -> std::io::Result<std::process::ExitStatus> {
        if let Some(mut child) = self.child.take() {
            child.wait().await
        } else {
            Err(std::io::Error::other("child already reaped"))
        }
    }

    /// Get a mutable reference to the inner child (for operations like start_kill).
    pub fn get_mut(&mut self) -> Option<&mut Child> {
        self.child.as_mut()
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Child was not waited on - kill it to prevent orphaning.
            // Best-effort: ignore errors if already dead.
            let _ = child.start_kill();
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// ProcessGroupKillGuard
// ──────────────────────────────────────────────────────────────────────────────

/// Kills a spawned process's group on drop unless [`disarm`](Self::disarm) was
/// called first.
///
/// A `tokio::time::timeout` wrapped directly around a spawn-and-wait future
/// (e.g. `Command::output()`) kills the child correctly when *that* timeout
/// fires: the future resolves to `Err(Elapsed)` and the caller's own match
/// arm can react. The gap is a timeout wrapping something *further up* the
/// call stack — Worker's mitosis-evaluation step wraps a whole multi-step
/// `evaluate()` call (prompt build, dispatch, response parse) in its own,
/// shorter, `tokio::time::timeout`; the weave strand does the same around its
/// whole `evaluate_internal()`. When that outer timeout fires first, it drops
/// the in-flight future — including whatever spawn-and-wait future was
/// nested inside it — before any inner match arm, kill logic, or cleanup code
/// gets a chance to run. Dropping a future mid-`Child::wait()` does not kill
/// the OS process; the child is simply orphaned, silently, indefinitely. This
/// guard makes that outcome unreachable: however the future is torn down,
/// `Drop` runs and reaps the process group. See bf-653n7.
pub struct ProcessGroupKillGuard {
    pid: i32,
    armed: bool,
}

impl ProcessGroupKillGuard {
    /// `pid` must be the direct child's PID *and* its process group ID —
    /// i.e. the child must have been spawned with `setpgid(0, 0)` in a
    /// `pre_exec` hook, so a group-wide kill here reaches subprocesses the
    /// child itself forked (e.g. a shell exec'ing into a long-running CLI),
    /// not just the direct child, without ever touching an unrelated group
    /// (notably, the caller's own, if the child were left in the caller's
    /// inherited group instead of its own).
    pub fn new(pid: u32) -> Self {
        Self {
            pid: pid as i32,
            armed: pid > 0,
        }
    }

    /// Call once the process is confirmed reaped (normal exit, or after the
    /// caller's own manual kill+wait) so `Drop` becomes a no-op instead of
    /// signaling a process-group ID the kernel may since have reused.
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupKillGuard {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: killpg with SIGKILL on a plain pid_t is not memory-unsafe;
            // libc::kill/killpg calls are marked unsafe only because they are
            // FFI. Best-effort: ESRCH (already dead) is expected and ignored.
            unsafe {
                libc::killpg(self.pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}
