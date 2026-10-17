//! tmux invocation helpers.
//!
//! Reading `NEEDLE_TMUX_SOCKET` lets integration tests point every tmux
//! invocation in this crate at an isolated socket instead of the default
//! one, so test-spawned sessions (and any `needle` subprocess a test
//! invokes) can never collide with — or kill — a real production fleet's
//! sessions running on the same box. The env var is never set in a real
//! deployment, so `command()` behaves exactly like `Command::new("tmux")`
//! there.
//!
//! [`run_with_timeout`] is the bounded counterpart for the client commands
//! status, list, and cleanup rely on: a tmux client waits on its server
//! forever, so a wedged server would otherwise hang those commands too.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

/// Build a `tmux` `Command`, honoring `NEEDLE_TMUX_SOCKET` if set.
///
/// Callers should chain their usual subcommand args after this, e.g.
/// `tmux_socket::command().args(["new-session", "-d", "-s", name, cmd])`.
/// The `-L <socket>` arguments (when present) are added first, matching
/// tmux's requirement that client options precede the subcommand.
pub fn command() -> Command {
    let mut cmd = Command::new("tmux");
    if let Ok(socket) = std::env::var("NEEDLE_TMUX_SOCKET") {
        cmd.args(["-L", &socket]);
    }
    cmd
}

/// Default per-invocation bound for tmux client commands.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// Environment variable overriding [`DEFAULT_COMMAND_TIMEOUT`] (whole
/// seconds). Set it larger on a host whose tmux server is known to answer
/// slowly; there is deliberately no way to disable the bound.
pub const TIMEOUT_ENV: &str = "NEEDLE_TMUX_TIMEOUT_SECS";

/// The per-invocation bound for tmux client commands.
///
/// A tmux client waits on its server indefinitely, so a wedged tmux server
/// turns every tmux-backed command into a permanent hang — `needle status`
/// and `needle list` open with a `tmux list-sessions` call and were observed
/// blocking for over an hour this way. The bound is read per call so tests
/// can shorten it, but once set it always applies.
pub fn command_timeout() -> Duration {
    match std::env::var(TIMEOUT_ENV) {
        Ok(raw) => raw
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|secs| *secs > 0)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_COMMAND_TIMEOUT),
        Err(_) => DEFAULT_COMMAND_TIMEOUT,
    }
}

/// Run a tmux `Command` to completion with a hard wall-clock bound.
///
/// `Command::output()` waits for the child forever; polling `try_wait`
/// against a deadline bounds it. On expiry the child is killed and
/// `ErrorKind::TimedOut` is returned so callers can degrade (skip the tmux
/// view, keep the process-table view) instead of hanging.
///
/// The child's stdout/stderr are captured through pipes and only drained
/// after exit, so output larger than the OS pipe buffer would deadlock the
/// child into the timeout. Every caller here captures tiny outputs (a
/// session list, an empty success line), which is the documented
/// precondition for using this helper.
pub fn run_with_timeout(mut command: Command, timeout: Duration) -> std::io::Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(drain(child, status)),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("no exit within {timeout:?}"),
                ));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Read the exited child's pipes and assemble its `Output`.
fn drain(mut child: Child, status: ExitStatus) -> Output {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_end(&mut stderr);
    }
    Output {
        status,
        stdout,
        stderr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_with_timeout_captures_fast_command_output() {
        let mut command = Command::new("echo");
        command.arg("needle-status-bound");
        let output = run_with_timeout(command, Duration::from_secs(10)).expect("echo exits");

        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("needle-status-bound"),
            "stdout should carry the echo output"
        );
    }

    #[test]
    fn run_with_timeout_bounds_a_hung_command() {
        let started = Instant::now();
        let error = run_with_timeout(
            {
                let mut command = Command::new("sleep");
                command.arg("30");
                command
            },
            Duration::from_millis(150),
        )
        .expect_err("sleep 30 must not be allowed to finish");

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the bound must fire near the deadline, took {:?}",
            started.elapsed()
        );
    }
}
