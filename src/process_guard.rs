//! Process-group kill guard and exit polling for spawned agent subprocesses.
//!
//! Depends on: nothing (leaf module — only `libc` and `std`).

use std::process::Child as StdChild;
use std::thread;
use std::time::{Duration, Instant};
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

    /// Create a future that waits for the child to exit without consuming the guard.
    ///
    /// This allows using the guard in select! branches where the guard needs to remain
    /// available for other operations. The future will wait for the child to exit
    /// and return the exit status, but the guard remains owned by the caller.
    ///
    /// After this future completes successfully, call `wait()` to consume the guard
    /// and mark it as reaped, or drop the guard to trigger cleanup.
    pub async fn wait_borrowed(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if let Some(ref mut child) = self.child {
            child.wait().await
        } else {
            Err(std::io::Error::other("child already reaped"))
        }
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
// ProcessGuard (sync)
// ──────────────────────────────────────────────────────────────────────────────

/// Guard for a spawned child process that ensures cleanup on drop.
///
/// Wraps a `std::process::Child` and provides a `wait()` method. If dropped
/// before `wait()` is called, the child process is killed to prevent orphaning.
pub struct ProcessGuardSync {
    pub(crate) child: Option<StdChild>,
    pid: u32,
}

impl ProcessGuardSync {
    /// Create a new ProcessGuard from a spawned Child.
    ///
    /// # Arguments
    /// * `child` - The spawned child process to guard
    pub fn new(child: StdChild) -> Self {
        let pid = child.id();
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
    pub fn wait(mut self) -> std::io::Result<std::process::ExitStatus> {
        if let Some(mut child) = self.child.take() {
            child.wait()
        } else {
            Err(std::io::Error::other("child already reaped"))
        }
    }

    /// Get a mutable reference to the inner child (for operations like start_kill).
    pub fn get_mut(&mut self) -> Option<&mut StdChild> {
        self.child.as_mut()
    }
}

impl Drop for ProcessGuardSync {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Child was not waited on - kill it to prevent orphaning.
            // Best-effort: ignore errors if already dead.
            let _ = child.kill();
            let _ = child.wait();
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

// ──────────────────────────────────────────────────────────────────────────────
// Exit polling
// ──────────────────────────────────────────────────────────────────────────────

/// Interval between liveness checks while waiting for a killed process tree to
/// drain. Short enough that a reported survivor's PID has not had time to be
/// recycled, cheap enough that a full grace period costs at most a few hundred
/// `/proc` reads.
pub const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Wait for a set of PIDs to exit, polling at [`EXIT_POLL_INTERVAL`] up to
/// `grace_period`, and return the PIDs still alive when it expires.
///
/// Killed workers do not vanish on signal — they run shutdown handlers and
/// exit gracefully over the next few seconds. Verifying a kill by scanning
/// immediately afterwards therefore reports cleanly-dying processes as
/// survivors (GitHub #21), indistinguishable from real orphans. Callers
/// should pass the grace period they are willing to wait —
/// `stop.grace_period_secs` in config, default 10s — and treat only the
/// returned PIDs as still alive. A PID in the return value was alive at the
/// last poll inside the window, so it is safe to name in output; a PID that
/// exited is never returned, since its number can already have been recycled.
///
/// `is_alive` is injected so callers decide what counts as exited (zombies,
/// for instance, usually should not) and so tests can simulate a process
/// table without spawning real processes.
///
/// Returns immediately — without a single poll or sleep — when `pids` is
/// empty or every PID is already gone.
pub fn wait_for_exit(
    pids: &[u32],
    grace_period: Duration,
    is_alive: &dyn Fn(u32) -> bool,
) -> Vec<u32> {
    // A killed tree can contain the same PID twice (a session's root also
    // appearing in a snapshot, say); report each survivor once.
    let mut pending: Vec<u32> = Vec::with_capacity(pids.len());
    for &pid in pids {
        if !pending.contains(&pid) {
            pending.push(pid);
        }
    }

    let deadline = Instant::now() + grace_period;
    loop {
        pending.retain(|&pid| is_alive(pid));
        if pending.is_empty() {
            return Vec::new();
        }
        let now = Instant::now();
        if now >= deadline {
            return pending;
        }
        // Clip the final slice to the deadline so the wait is bounded by
        // `grace_period` regardless of how the interval and grace interact.
        thread::sleep(EXIT_POLL_INTERVAL.min(deadline - now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn wait_for_exit_returns_immediately_for_an_empty_set() {
        let is_alive = |_pid: u32| true;
        let start = Instant::now();
        let survivors = wait_for_exit(&[], Duration::from_secs(30), &is_alive);
        assert!(survivors.is_empty());
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "an empty PID set must not wait out the grace period"
        );
    }

    #[test]
    fn wait_for_exit_returns_immediately_when_everything_already_exited() {
        let is_alive = |_pid: u32| false;
        let start = Instant::now();
        let survivors = wait_for_exit(&[101, 202, 303], Duration::from_secs(30), &is_alive);
        assert!(survivors.is_empty());
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "a set that has already drained must not wait out the grace period"
        );
    }

    #[test]
    fn wait_for_exit_returns_only_survivors_after_the_deadline() {
        let alive = [202_u32];
        let is_alive = move |pid: u32| alive.contains(&pid);
        let start = Instant::now();
        let grace = Duration::from_millis(200);
        let survivors = wait_for_exit(&[101, 202, 303], grace, &is_alive);
        assert_eq!(survivors, vec![202]);
        assert!(
            start.elapsed() >= grace,
            "survivors must only be reported once the grace period has elapsed"
        );
    }

    #[test]
    fn wait_for_exit_returns_as_soon_as_the_set_drains() {
        // Alive for the first two polls, gone on the third: the helper must
        // come back well before a grace period that has not elapsed.
        let polls = AtomicUsize::new(0);
        let is_alive = move |_pid: u32| polls.fetch_add(1, Ordering::Relaxed) < 2;
        let grace = Duration::from_secs(30);
        let start = Instant::now();
        let survivors = wait_for_exit(&[404], grace, &is_alive);
        assert!(survivors.is_empty());
        assert!(
            start.elapsed() < grace,
            "a drained set must not wait out the grace period"
        );
    }

    #[test]
    fn wait_for_exit_reports_a_survivor_once_per_pid() {
        let is_alive = |_pid: u32| true;
        let survivors = wait_for_exit(&[505, 505, 606], Duration::ZERO, &is_alive);
        assert_eq!(survivors, vec![505, 606]);
    }
}
