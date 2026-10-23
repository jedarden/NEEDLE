//! Targeted reaping of zombie direct children in the worker process.
//!
//! The supervisor daemon reaps its exited workers with a blind
//! `waitpid(-1, WNOHANG)` sweep ([`crate::supervisor::reap_exited_child`]
//! exposes the same loop; ADR-010 / GH #12). That sweep is safe *only*
//! because `needle supervise` directly spawns nothing but workers. A worker
//! process is the opposite: it concurrently waits on the agent dispatch,
//! gate commands, verification runs, and telemetry subprocesses, so a blind
//! `-1` sweep here could steal an exit status a live `.wait()` is about to
//! consume — a std caller would see a spurious `ECHILD`, and a tokio waiter
//! would never complete.
//!
//! This module therefore reaps **by specific PID**: it scans `/proc` for
//! entries whose parent is this process and whose state is `Z`, then issues
//! one `waitpid(pid, WNOHANG)` per hit. A process visible as `Z` is already
//! dead, so the sweep can never disturb a running child. A zombie that
//! appears here is by definition unclaimed at scan time; the sweep runs at
//! dispatch teardown, *after every child handle the dispatch holds* (the
//! agent on every branch, the output transform in its teardown) has been
//! reaped. Placement inside that window matters: an exited-but-unwaited
//! transform is an unreaped zombie from the moment its stdout EOF completes
//! the readers until its guard waits on it, and a sweep inside that window
//! would reap it out from under the tokio SIGCHLD driver — the driver
//! resolves `wait` futures by pid, so its waiter would never resolve and
//! dispatch would hang. Even so, the window where a concurrently-waited
//! child could be visible as a zombie only shrinks to the runtime's own
//! reap latency; it is never zero, which is why the sweep is a teardown
//! cleanup and not a substitute for each spawn site reaping its own child.
//!
//! The observable goal: a bead cycle ends with zero `<defunct>` entries
//! under the worker, so external governors that refuse to retire a worker
//! while it has children (e.g. subgov `stop_idle`) are never pinned to an
//! idle worker by process-table debris (needle-064b92ef).

/// Collect the PIDs of this process's direct children that are zombies.
///
/// A zombie is an already-exited child whose parent has not reaped it; it
/// occupies a process-table entry and a PID slot until `waitpid` collects
/// it. Only `/proc` entries whose `PPid` equals `parent_pid` and whose
/// state is `Z` are returned, sorted for deterministic logs and tests.
#[cfg(target_os = "linux")]
pub fn zombie_children(parent_pid: u32) -> Vec<u32> {
    let mut zombies = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return zombies;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid == parent_pid {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue; // The process exited between readdir and stat.
        };
        if let Some((state, ppid)) = parse_stat_fields(&stat) {
            if state == 'Z' && ppid == parent_pid {
                zombies.push(pid);
            }
        }
    }
    zombies.sort_unstable();
    zombies
}

/// Parse the `(state, ppid)` tail of a `/proc/<pid>/stat` line.
///
/// The comm field is parenthesized and may itself contain spaces and
/// parentheses, so the fixed-width fields are located from the *last* `)`
/// rather than by splitting the whole line; `state` (field 3) and `ppid`
/// (field 4) are the first two whitespace-separated fields after it.
#[cfg(target_os = "linux")]
fn parse_stat_fields(stat: &str) -> Option<(char, u32)> {
    let tail = &stat[stat.rfind(')')? + 1..];
    let mut fields = tail.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    Some((state, ppid))
}

/// Reap every zombie direct child of this process.
///
/// Returns the PIDs that were collected. `WNOHANG` on an already-zombie PID
/// never blocks, and a PID that stopped being a zombie between the `/proc`
/// scan and the `waitpid` (reaped elsewhere) simply does not match and is
/// skipped — the sweep is idempotent under concurrency.
#[cfg(target_os = "linux")]
pub fn reap_zombie_children_of_self() -> Vec<u32> {
    let reaped: Vec<u32> = zombie_children(std::process::id())
        .into_iter()
        .filter(|pid| {
            let mut status: libc::c_int = 0;
            // SAFETY: waitpid reaps an already-exited direct child without
            // blocking; `status` is a valid stack-local out parameter for the
            // duration of the call.
            let waited = unsafe { libc::waitpid(*pid as libc::pid_t, &mut status, libc::WNOHANG) };
            waited == *pid as libc::pid_t
        })
        .collect();
    if !reaped.is_empty() {
        tracing::warn!(
            reaped = ?reaped,
            "reaped zombie child processes left unreaped in the worker"
        );
    }
    reaped
}

/// Non-Linux platforms have no `/proc` to scan; nothing to sweep.
#[cfg(not(target_os = "linux"))]
pub fn reap_zombie_children_of_self() -> Vec<u32> {
    Vec::new()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn parse_stat_fields_reads_state_and_ppid_past_a_parenthesized_comm() {
        // comm containing spaces and parentheses must not skew the fields.
        let stat = "3428272 (git ) def) Z 3428000 3428272 3428000 0 -1";
        assert_eq!(parse_stat_fields(stat), Some(('Z', 3428000)));

        let stat = "1234 (bash) S 5678 1234 5678 0 -1";
        assert_eq!(parse_stat_fields(stat), Some(('S', 5678)));

        // A live entry must never read as a zombie via the parser.
        let stat = "99 (sleep) S 1 99 1 0 -1";
        assert_eq!(parse_stat_fields(stat).map(|(state, _)| state), Some('S'));
    }

    #[test]
    fn parse_stat_fields_rejects_malformed_lines() {
        assert_eq!(parse_stat_fields("no parens here"), None);
        assert_eq!(parse_stat_fields("12 (x)"), None);
        assert_eq!(parse_stat_fields("12 (x) Z not-a-pid"), None);
        assert_eq!(parse_stat_fields(""), None);
    }

    #[test]
    fn zombie_children_ignores_malformed_and_self_entries() {
        // `/proc/self` does not parse as a numeric PID, and this process is
        // not its own child; both must be skipped without error.
        let zombies = zombie_children(std::process::id());
        assert!(!zombies.contains(&std::process::id()));
    }
}
