//! Test-only tmux socket override.
//!
//! Reading `NEEDLE_TMUX_SOCKET` lets integration tests point every tmux
//! invocation in this crate at an isolated socket instead of the default
//! one, so test-spawned sessions (and any `needle` subprocess a test
//! invokes) can never collide with — or kill — a real production fleet's
//! sessions running on the same box. The env var is never set in a real
//! deployment, so `command()` behaves exactly like `Command::new("tmux")`
//! there.

use std::process::Command;

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
