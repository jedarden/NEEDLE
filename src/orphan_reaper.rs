//! Detection and reaping of worker-spawned processes that escaped containment.
//!
//! Depends on: nothing (leaf module — only `libc` and `std`).
//!
//! # The problem
//!
//! A worker's agent subprocess is spawned with its own process group
//! (`setpgid(0,0)`) and is killed group-wide on timeout and on drop
//! ([`crate::process_guard::ProcessGroupKillGuard`]). That kill path cannot
//! reach everything the agent's work spawns. When a test command runs under a
//! transient systemd scope — `systemd-run --user --scope ...` — the payload
//! moves into a *new* scope and process group. When the agent exits or is
//! killed, those processes are reparented to `systemd --user`, outlive the
//! worker indefinitely, and were observed holding ~24 GB and saturating the
//! box for 13.5 hours while their repo had no live worker (bead
//! needle-092bae5d, incident 2026-09-07; the mta-my-way vitest forks lived 9+
//! days).
//!
//! # The fix, in two layers
//!
//! 1. **Attribution + post-dispatch reap.** Every dispatch injects marker env
//!    vars ([`DISPATCH_ENV`], [`WORKER_ENV`], [`WORKSPACE_ENV`]) into the
//!    agent's environment. `systemd-run --scope` payloads inherit the caller's
//!    environment, so anything the agent spawns carries the markers. When the
//!    agent exits — normally, on timeout, or on error — dispatch scans
//!    `/proc` for survivors carrying this dispatch's marker and terminates
//!    them (see `Dispatcher::reap_dispatch_escapees`).
//! 2. **Periodic safety net.** A worker killed by SIGKILL cannot run its own
//!    cleanup, and processes spawned by workers running binaries from before
//!    the markers existed carry no marker at all. The worker therefore
//!    periodically sweeps `/proc` for processes rooted in a `run-p*.scope`
//!    transient scope whose workspace has no live worker and whose age
//!    exceeds the configured threshold, and terminates them
//!    ([`sweep_orphans`]).
//!
//! Containment itself (worker processes landing inside `needle.slice`) is
//! inherited from how the worker is launched: `systemd-run --scope` without
//! an explicit `--slice` places the scope in the *caller's* slice, so a
//! worker running under `needle-worker@.service` in `needle.slice` produces
//! child scopes that are still inside `needle.slice` and bounded by its
//! MemoryMax. The reaper exists because containment alone still leaks a
//! capped-but-immortal process, and because nothing prevented a worker from
//! being launched outside the slice entirely (warned once at dispatch via
//! [`warn_if_outside_needle_slice`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Dispatch-scoped attribution marker injected into the agent environment.
///
/// Any process carrying this value in `/proc/<pid>/environ` was spawned (at
/// any depth) by this specific dispatch and is an escapee the moment the
/// dispatch's own process group is gone.
pub const DISPATCH_ENV: &str = "NEEDLE_DISPATCH_ID";

/// Worker-identity attribution marker injected into the agent environment.
pub const WORKER_ENV: &str = "NEEDLE_WORKER_ID";

/// Workspace attribution marker injected into the agent environment.
pub const WORKSPACE_ENV: &str = "NEEDLE_WORKSPACE";

// ──────────────────────────────────────────────────────────────────────────────
// Escapee model
// ──────────────────────────────────────────────────────────────────────────────

/// A process that has escaped containment, as observed in `/proc`.
#[derive(Debug, Clone)]
pub struct Escapee {
    pub pid: u32,
    /// Process group ID. Transient-scope members share the scope root's group,
    /// so this names the tree the escapee actually belongs to.
    pub pgid: u32,
    /// Seconds since the process started, from `/proc/<pid>/stat` starttime.
    pub age_secs: u64,
    /// [`DISPATCH_ENV`] value, when the process carries the marker.
    pub dispatch_id: Option<String>,
    /// [`WORKSPACE_ENV`] value, when the process carries the marker.
    pub marked_workspace: Option<PathBuf>,
    /// Current working directory — the workspace fallback for unmarked
    /// processes spawned before markers existed.
    pub cwd: Option<PathBuf>,
    /// The `run-p<pid>-i<id>.scope` unit this process is rooted in, when any.
    pub scope_unit: Option<String>,
    /// Command line, NUL bytes replaced with spaces (for logs and reports).
    pub cmdline: String,
}

impl Escapee {
    /// Best-effort workspace this escapee belongs to: the marked workspace if
    /// the process carries the marker, else its cwd.
    pub fn workspace(&self) -> Option<&Path> {
        self.marked_workspace.as_deref().or(self.cwd.as_deref())
    }
}

/// Outcome of a reap pass.
#[derive(Debug, Default, Clone)]
pub struct ReapReport {
    /// PIDs signalled with SIGTERM (includes every PID later SIGKILLed).
    pub terminated: Vec<u32>,
    /// PIDs that ignored SIGTERM for the full grace period and were SIGKILLed.
    pub killed: Vec<u32>,
    /// PIDs that survived even SIGKILL (should be unreachable; reported so a
    /// survivor is a named fact, not a silence).
    pub survived: Vec<u32>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Reaper
// ──────────────────────────────────────────────────────────────────────────────

/// Scans and reaps escaped worker-spawned processes.
///
/// All reads go through `proc_root` so tests can point this at a synthetic
/// `/proc` tree; [`OrphanReaper::system`] is the production constructor.
pub struct OrphanReaper {
    proc_root: PathBuf,
}

impl OrphanReaper {
    /// Production reaper reading the real `/proc`.
    pub fn system() -> Self {
        Self::with_proc_root("/proc")
    }

    /// Reaper reading a (possibly synthetic) proc root.
    pub fn with_proc_root(root: impl Into<PathBuf>) -> Self {
        Self {
            proc_root: root.into(),
        }
    }

    /// Every live process rooted in a `run-p*.scope` transient scope.
    ///
    /// This is the periodic safety net's candidate set: transient scopes are
    /// the shape `systemd-run --scope` produces, and they are exactly the
    /// processes a worker's process-group kill cannot reach. Ordinary worker
    /// descendants (in `needle-worker@.service` or plain process groups) never
    /// match, so the sweep cannot see anything a worker still controls.
    pub fn scan_scope_escapees(&self) -> Vec<Escapee> {
        let mut found = Vec::new();
        for pid in self.numeric_pids() {
            let Ok(cgroup) = std::fs::read_to_string(self.proc_root.join(&pid).join("cgroup"))
            else {
                continue;
            };
            let Some(scope_unit) = scope_unit_from_cgroup(&cgroup) else {
                continue;
            };
            if let Some(escapee) = self.read_escapee(&pid, scope_unit) {
                found.push(escapee);
            }
        }
        found
    }

    /// Every live process carrying `dispatch_id` in its environment.
    ///
    /// Not restricted to scope-rooted processes: a test runner an agent
    /// `nohup`ed or backgrounded is as much an escapee as one a
    /// `systemd-run --scope` hid in a transient scope, and all of them carry
    /// the dispatch marker by inheritance.
    ///
    /// Needle worker processes are excluded even when they carry the marker:
    /// a worker nested inside an agent session (an operator test, mitosis)
    /// inherits its parent's environment, and `/proc/<pid>/environ` reflects
    /// the exec-time environment, not later `set_var` calls — the only
    /// reliable guard is the binary name itself.
    pub fn scan_dispatch_escapees(&self, dispatch_id: &str) -> Vec<Escapee> {
        let mut found = Vec::new();
        for pid in self.numeric_pids() {
            if is_self(&pid) || is_needle_process(&self.proc_root, &pid) {
                continue;
            }
            let Ok(env) = read_environ(&self.proc_root.join(&pid).join("environ")) else {
                continue;
            };
            if env.get(DISPATCH_ENV).map(String::as_str) != Some(dispatch_id) {
                continue;
            }
            if let Some(escapee) = self.read_escapee(&pid, None) {
                found.push(escapee);
            }
        }
        found
    }

    /// Workspaces that currently have a live needle worker process, by
    /// `--workspace` argument. Escapees whose workspace is in this set are
    /// left alone — a worker on that workspace owns whatever is running there.
    pub fn live_worker_workspaces(&self) -> HashSet<PathBuf> {
        let mut live = HashSet::new();
        for pid in self.numeric_pids() {
            if !is_needle_process(&self.proc_root, &pid) {
                continue;
            }
            let Ok(cmdline) = read_cmdline(&self.proc_root.join(&pid).join("cmdline")) else {
                continue;
            };
            if let Some(ws) = workspace_from_cmdline(&cmdline) {
                if let Ok(canonical) = std::fs::canonicalize(&ws) {
                    live.insert(canonical);
                } else {
                    live.insert(ws);
                }
            }
        }
        live
    }

    /// SIGTERM every escapee, wait `term_grace` for them to drain, then
    /// SIGKILL whatever is left.
    ///
    /// SIGTERM first is deliberate: the 2026-09-07 incident was cleared with
    /// SIGTERM alone (vitest shuts down cleanly given the chance), and a
    /// graceful stop lets test runners flush partial results and release
    /// locks. PID liveness is evaluated against this reaper's proc root, and
    /// zombies count as dead — a zombie holds no resources the box can feel.
    pub fn reap(&self, escapees: &[Escapee], term_grace: Duration) -> ReapReport {
        let mut report = ReapReport::default();
        if escapees.is_empty() {
            return report;
        }

        for pid in escapees.iter().map(|e| e.pid) {
            // Safety: kill is FFI; ESRCH (already dead) is expected and ignored.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            report.terminated.push(pid);
        }

        let is_alive = |pid: u32| !process_is_gone(&self.proc_root, pid);
        let survivors =
            crate::process_guard::wait_for_exit(&report.terminated, term_grace, &is_alive);
        if survivors.is_empty() {
            return report;
        }

        for pid in &survivors {
            // Safety: kill is FFI; ESRCH (already dead) is expected and ignored.
            unsafe {
                libc::kill(*pid as libc::pid_t, libc::SIGKILL);
            }
            report.killed.push(*pid);
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        for pid in survivors {
            while process_alive(&self.proc_root, pid) && std::time::Instant::now() < deadline {
                std::thread::sleep(crate::process_guard::EXIT_POLL_INTERVAL);
            }
            if process_alive(&self.proc_root, pid) {
                report.survived.push(pid);
            }
        }
        report
    }

    /// Read one process's escapee-relevant attributes. Returns `None` when the
    /// process vanished mid-scan, is a zombie, is this process, or is itself a
    /// needle worker.
    fn read_escapee(&self, pid: &str, scope_unit: Option<String>) -> Option<Escapee> {
        if is_self(pid) || is_needle_process(&self.proc_root, pid) {
            return None;
        }
        let dir = self.proc_root.join(pid);
        let stat = std::fs::read_to_string(dir.join("stat")).ok()?;
        let (pgid, starttime_secs) = parse_stat_pgid_starttime(&stat)?;
        if process_state(&stat) == Some('Z') {
            return None;
        }
        let env = read_environ(&dir.join("environ")).unwrap_or_default();
        let cwd = std::fs::read_link(dir.join("cwd")).ok();
        let cmdline = read_cmdline(&dir.join("cmdline")).unwrap_or_default();
        let age_secs = process_age_secs(&self.proc_root, starttime_secs)?;
        Some(Escapee {
            pid: pid.parse().ok()?,
            pgid,
            age_secs,
            dispatch_id: env.get(DISPATCH_ENV).cloned(),
            marked_workspace: env.get(WORKSPACE_ENV).map(PathBuf::from),
            cwd,
            scope_unit,
            cmdline,
        })
    }

    fn numeric_pids(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.proc_root) else {
            return Vec::new();
        };
        let mut pids: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.bytes().all(|b| b.is_ascii_digit()) && !name.is_empty())
            .collect();
        pids.sort();
        pids
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Sweep
// ──────────────────────────────────────────────────────────────────────────────

/// One periodic safety-net pass: scope-rooted processes old enough, on a
/// workspace with no live worker, get reaped.
///
/// `own_workspace` is always protected even if the caller's own registration
/// were missed by the live-worker scan — a worker must never reap something
/// its own next dispatch might still own.
pub fn sweep_orphans(
    reaper: &OrphanReaper,
    orphan_min_age_secs: u64,
    own_workspace: &Path,
    term_grace: Duration,
) -> (Vec<Escapee>, ReapReport) {
    let candidates = reaper.scan_scope_escapees();
    if candidates.is_empty() {
        return (candidates, ReapReport::default());
    }

    let live = reaper.live_worker_workspaces();
    let own = std::fs::canonicalize(own_workspace).unwrap_or_else(|_| own_workspace.to_path_buf());

    let victims: Vec<Escapee> = candidates
        .into_iter()
        .filter(|e| e.age_secs >= orphan_min_age_secs)
        .filter(|e| {
            e.workspace()
                .map(|ws| {
                    let canonical = std::fs::canonicalize(ws).unwrap_or_else(|_| ws.to_path_buf());
                    canonical != own && !live.contains(&canonical)
                })
                // No workspace attribution at all: nothing ties this process
                // to any worker, live or dead. Leave it; it is not the escapee
                // shape this sweep exists for, and guessing by scope alone
                // would eventually reap something a human started.
                .unwrap_or(false)
        })
        .collect();

    if victims.is_empty() {
        return (victims, ReapReport::default());
    }

    let report = reaper.reap(&victims, term_grace);
    (victims, report)
}

/// Generate a dispatch-scoped attribution marker.
///
/// Worker id plus millisecond timestamp plus a process-wide sequence: unique
/// across dispatches of a worker (the unit every reap is scoped to), and
/// stable enough to grep for.
pub fn generate_dispatch_id(worker_id: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{worker_id}-{millis}-{seq}")
}

/// Warn once per worker process when the worker is not running inside
/// `needle.slice`.
///
/// Containment is inherited from the launcher: `systemd-run --scope` places
/// the scope in the caller's slice, so everything a contained worker spawns —
/// including agent-created child scopes — stays under the slice's
/// MemoryMax/CPUQuota. A worker launched outside it silently loses all of
/// that, which is the 2026-08-23 protection not being in the path at all.
/// This makes that mislaunch a named warning instead of a silent one.
pub fn warn_if_outside_needle_slice() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    match current_cgroup_slice(Path::new("/proc/self")) {
        Some(slice) if slice == "needle.slice" => {
            tracing::debug!(slice = %slice, "worker running inside needle.slice");
        }
        Some(slice) => {
            tracing::warn!(
                slice = %slice,
                "worker is NOT running inside needle.slice — spawned processes (including \
                 agent-created systemd-run scopes) will escape the fleet's cgroup caps; \
                 launch workers under needle-worker@.service"
            );
        }
        None => {
            tracing::debug!("no systemd slice detected for worker (container or bare shell)");
        }
    }
}

/// The last `*.slice` component of this process's cgroup path, if any.
fn current_cgroup_slice(self_proc: &Path) -> Option<String> {
    let cgroup = std::fs::read_to_string(self_proc.join("cgroup")).ok()?;
    cgroup
        .lines()
        .flat_map(|line| line.splitn(2, "::").nth(1))
        .flat_map(|path| path.split('/'))
        .filter(|c| c.ends_with(".slice"))
        .last()
        .map(str::to_owned)
}

// ──────────────────────────────────────────────────────────────────────────────
// /proc parsing helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Extract the `run-p<pid>-i<id>.scope` unit name from `/proc/<pid>/cgroup`
/// content, when the process is rooted in a transient scope.
fn scope_unit_from_cgroup(cgroup: &str) -> Option<String> {
    cgroup.lines().find_map(|line| {
        line.split('/').find_map(|component| {
            let unit = component.strip_suffix(".scope")?;
            let rest = unit.strip_prefix("run-p")?;
            let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
            if digits == 0 {
                return None;
            }
            let after = &rest[digits..];
            if after.starts_with("-i") {
                Some(component.to_owned())
            } else {
                None
            }
        })
    })
}

/// Parse `/proc/<pid>/environ` into a map. Trailing NUL, entries without an
/// `=`, and read races mid-exec are all tolerated.
fn read_environ(path: &Path) -> std::io::Result<HashMap<String, String>> {
    let bytes = std::fs::read(path)?;
    let mut map = HashMap::new();
    for entry in bytes.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let entry = String::from_utf8_lossy(entry);
        if let Some((key, value)) = entry.split_once('=') {
            map.insert(key.to_owned(), value.to_owned());
        }
    }
    Ok(map)
}

/// Read `/proc/<pid>/cmdline`, NUL bytes replaced with spaces.
fn read_cmdline(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes)
        .replace('\0', " ")
        .trim()
        .to_owned())
}

/// `(pgid, starttime_secs_after_boot)` from `/proc/<pid>/stat`.
///
/// The comm field is parenthesized and may contain spaces and parens itself,
/// so parsing starts after the *last* `)`. In that remainder, `state` is
/// field 3 overall (index 0), `pgid` field 5 (index 2), `starttime` field 22
/// (index 19), all in clock ticks.
fn parse_stat_pgid_starttime(stat: &str) -> Option<(u32, u64)> {
    let rest = stat.rsplit_once(')')?.1.trim_start();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let pgid = fields.get(2)?.parse().ok()?;
    let starttime_ticks: u64 = fields.get(19)?.parse().ok()?;
    Some((pgid, starttime_ticks / clock_ticks_per_sec()))
}

/// The process state letter from `/proc/<pid>/stat` (`R`, `S`, `Z`, …).
fn process_state(stat: &str) -> Option<char> {
    let rest = stat.rsplit_once(')')?.1.trim_start();
    rest.chars().next()
}

fn clock_ticks_per_sec() -> u64 {
    // Safety: sysconf is FFI but has no preconditions; a 0/err result falls
    // back to the Linux default of 100.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks > 0 {
        ticks as u64
    } else {
        100
    }
}

/// Seconds since boot for a process whose stat reported `starttime` in ticks.
fn process_age_secs(proc_root: &Path, starttime_ticks: u64) -> Option<u64> {
    let uptime = std::fs::read_to_string(proc_root.join("uptime")).ok()?;
    let uptime_secs: f64 = uptime.split_whitespace().next()?.parse().ok()?;
    let started_at = starttime_ticks as f64 / clock_ticks_per_sec() as f64;
    if uptime_secs < started_at {
        return None;
    }
    Some(uptime_secs as u64 - started_at as u64)
}

/// True when the pid is this process.
fn is_self(pid: &str) -> bool {
    pid == std::process::id().to_string()
}

/// True when the process's executable basename starts with `needle` — i.e. it
/// is a needle worker binary and never an escapee.
fn is_needle_process(proc_root: &Path, pid: &str) -> bool {
    match std::fs::read_link(proc_root.join(pid).join("exe")) {
        Ok(exe) => exe
            .file_name()
            .map(|name| name.to_string_lossy().starts_with("needle"))
            .unwrap_or(false),
        // An unreadable exe link (exited mid-scan, or permissions) is not
        // evidence of a needle binary; treat as non-needle and let the
        // marker/cwd checks decide.
        Err(_) => false,
    }
}

/// `--workspace <path>` or `--workspace=<path>` from a worker cmdline.
fn workspace_from_cmdline(cmdline: &str) -> Option<PathBuf> {
    let tokens: Vec<&str> = cmdline.split_whitespace().collect();
    for (i, token) in tokens.iter().enumerate() {
        if let Some(ws) = token.strip_prefix("--workspace=") {
            return Some(PathBuf::from(ws));
        }
        if *token == "--workspace" {
            return tokens.get(i + 1).map(PathBuf::from);
        }
    }
    None
}

/// True when the pid does not correspond to a live, non-zombie process.
fn process_is_gone(proc_root: &Path, pid: u32) -> bool {
    !process_alive(proc_root, pid)
}

fn process_alive(proc_root: &Path, pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(proc_root.join(pid.to_string()).join("stat")) else {
        return false;
    };
    // A zombie holds a pid slot but nothing else; the parent that could reap
    // it is gone by definition here, so treat it as dead.
    process_state(&stat) != Some('Z')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake `/proc` entry. Returns the process directory.
    fn fake_pid(root: &Path, pid: &str, cgroup: Option<&str>, environ: &[(&str, &str)]) -> PathBuf {
        let dir = root.join(pid);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(cgroup) = cgroup {
            std::fs::write(dir.join("cgroup"), cgroup).unwrap();
        }
        let env: String = environ.iter().map(|(k, v)| format!("{k}={v}\0")).collect();
        std::fs::write(dir.join("environ"), env).unwrap();
        std::fs::write(dir.join("stat"), fake_stat(pid, 1000)).unwrap();
        std::fs::write(dir.join("cmdline"), "sleep\03000\0".to_string()).unwrap();
        dir
    }

    /// A syntactically valid `/proc/<pid>/stat` line: `state ppid pgrp` after
    /// the comm, `starttime` (field 22) = `starttime_ticks` at index 19 of the
    /// post-comm fields.
    fn fake_stat(pid: &str, starttime_ticks: u64) -> String {
        let mut fields = vec!["S".to_string(), "1".to_string(), "1".to_string()];
        // Fields 6..=21 overall → indices 3..=18 post-comm.
        for _ in 3..=18 {
            fields.push("0".to_string());
        }
        fields.push(starttime_ticks.to_string());
        // Fields 23..=52 — presence is enough; values are irrelevant here.
        for _ in 23..=52 {
            fields.push("0".to_string());
        }
        format!("{pid} (sleeper) {} \n", fields.join(" "))
    }

    fn write_uptime(root: &Path, secs: f64) {
        std::fs::write(root.join("uptime"), format!("{secs} 0.00\n")).unwrap();
    }

    #[test]
    fn scope_unit_is_detected_in_v2_cgroup_line() {
        let cgroup = "0::/user.slice/user-1000.slice/user@1000.service/needle.slice/run-p1101887-i9487900.scope\n";
        assert_eq!(
            scope_unit_from_cgroup(cgroup).as_deref(),
            Some("run-p1101887-i9487900.scope")
        );
    }

    #[test]
    fn ordinary_cgroups_are_not_scopes() {
        assert_eq!(scope_unit_from_cgroup("0::/user.slice/user-1000.slice/user@1000.service/needle.slice/needle-worker@glm-needle.service\n"), None);
        assert_eq!(
            scope_unit_from_cgroup("0::/system.slice/sshd.service\n"),
            None
        );
        assert_eq!(scope_unit_from_cgroup(""), None);
    }

    #[test]
    fn scope_lookalikes_are_rejected() {
        // Missing the -i invocation segment, or missing digits after run-p.
        assert_eq!(scope_unit_from_cgroup("0::/run-pabc.scope\n"), None);
        assert_eq!(scope_unit_from_cgroup("0::/run-p123.scope\n"), None);
        assert_eq!(
            scope_unit_from_cgroup("0::/run-p123-i9.scope").as_deref(),
            Some("run-p123-i9.scope")
        );
    }

    #[test]
    fn environ_parses_values_and_skips_malformed_entries() {
        let dir = std::env::temp_dir().join(format!("needle-orphan-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("environ");
        std::fs::write(&path, b"A=1\0B=with=equals\0NOEQUALS\0TRAILING\0").unwrap();
        let env = read_environ(&path).unwrap();
        assert_eq!(env.get("A").map(String::as_str), Some("1"));
        assert_eq!(env.get("B").map(String::as_str), Some("with=equals"));
        assert!(!env.contains_key("NOEQUALS"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stat_parsing_survives_spaces_in_comm() {
        let stat = "123 (node --experi mental) S 1 1101887 1101887 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 999900 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
        let (pgid, starttime) = parse_stat_pgid_starttime(stat).unwrap();
        assert_eq!(pgid, 1101887);
        assert_eq!(starttime, 9999);
        assert_eq!(process_state(stat), Some('S'));
    }

    #[test]
    fn age_is_uptime_minus_starttime() {
        let dir = std::env::temp_dir().join(format!("needle-orphan-age-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write_uptime(&dir, 100_000.0);
        // starttime ticks 999900 / 100 = 9999s after boot → age 90000.
        assert_eq!(process_age_secs(&dir, 999_900), Some(90_000));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_scope_escapees_finds_only_scope_rooted_processes() {
        let root = std::env::temp_dir().join(format!("needle-orphan-scan-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        write_uptime(&root, 100_000.0);

        fake_pid(
            &root,
            "111",
            Some(
                "0::/user.slice/user-1000.slice/user@1000.service/needle.slice/run-p123-i9.scope\n",
            ),
            &[("NEEDLE_WORKSPACE", "/home/coding/mta-my-way")],
        );
        // Same slice, but a worker service, not a transient scope.
        fake_pid(
            &root,
            "222",
            Some("0::/user.slice/user-1000.slice/user@1000.service/needle.slice/needle-worker@x.service\n"),
            &[],
        );
        // No cgroup at all.
        fake_pid(&root, "333", None, &[]);

        let reaper = OrphanReaper::with_proc_root(&root);
        let escapees = reaper.scan_scope_escapees();
        assert_eq!(
            escapees.len(),
            1,
            "only the transient-scope process matches"
        );
        assert_eq!(escapees[0].pid, 111);
        assert_eq!(escapees[0].scope_unit.as_deref(), Some("run-p123-i9.scope"));
        assert_eq!(
            escapees[0].marked_workspace.as_deref(),
            Some(Path::new("/home/coding/mta-my-way"))
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scan_dispatch_escapees_matches_marker_and_skips_needle_processes() {
        let root = std::env::temp_dir().join(format!("needle-orphan-disp-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        write_uptime(&root, 100_000.0);

        fake_pid(
            &root,
            "444",
            Some("0::/user.slice/run-p1-i1.scope\n"),
            &[("NEEDLE_DISPATCH_ID", "w-42"), ("NEEDLE_WORKER_ID", "w")],
        );
        // A nested needle worker inherits the parent dispatch's env — never a victim.
        let nested = fake_pid(
            &root,
            "555",
            Some("0::/user.slice/run-p1-i1.scope\n"),
            &[("NEEDLE_DISPATCH_ID", "w-42")],
        );
        std::fs::write(nested.join("cmdline"), "needle\0run\0--workspace\0/x\0").unwrap();
        // Symlink exe so is_needle_process sees the binary name.
        std::fs::write(root.join("needle-bin"), "#!/bin/sh\n").unwrap();
        let _ = std::os::unix::fs::symlink(root.join("needle-bin"), nested.join("exe"));
        // Wrong marker.
        fake_pid(
            &root,
            "666",
            Some("0::/user.slice/run-p2-i2.scope\n"),
            &[("NEEDLE_DISPATCH_ID", "other")],
        );

        let reaper = OrphanReaper::with_proc_root(&root);
        let escapees = reaper.scan_dispatch_escapees("w-42");
        assert_eq!(
            escapees.len(),
            1,
            "marker match only, needle processes excluded"
        );
        assert_eq!(escapees[0].pid, 444);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn live_worker_workspaces_reads_the_workspace_argument() {
        let root = std::env::temp_dir().join(format!("needle-orphan-live-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        write_uptime(&root, 100_000.0);

        let worker = fake_pid(&root, "777", None, &[]);
        std::fs::write(
            worker.join("cmdline"),
            "needle\0run\0--workspace\0/home/coding/ARMOR\0--agent\0claude\0",
        )
        .unwrap();
        std::fs::write(root.join("needle"), "#!/bin/sh\n").unwrap();
        let _ = std::os::unix::fs::symlink(root.join("needle"), worker.join("exe"));

        // A non-needle process on the same workspace does not make it live.
        let impostor = fake_pid(&root, "888", None, &[]);
        std::fs::write(
            impostor.join("cmdline"),
            "vitest\0run\0--workspace\0/home/coding/ARMOR\0",
        )
        .unwrap();

        // Resume-mode workers name no workspace → contribute nothing.
        let resume = fake_pid(&root, "999", None, &[]);
        std::fs::write(
            resume.join("cmdline"),
            "needle-stable\0run\0--resume\0--identifier\0x\0",
        )
        .unwrap();
        let _ = std::os::unix::fs::symlink(root.join("needle"), resume.join("exe"));

        let live = OrphanReaper::with_proc_root(&root).live_worker_workspaces();
        assert_eq!(live.len(), 1, "{live:?}");
        assert!(live.contains(Path::new("/home/coding/ARMOR")));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sweep_skips_young_processes_and_live_workspaces() {
        let root = std::env::temp_dir().join(format!("needle-orphan-sweep-{}", std::process::id()));
        std::fs::create_dir_all(&root.join("home").join("dead-repo")).unwrap();
        write_uptime(&root, 100_000.0);

        let cgroup = "0::/user.slice/run-p1-i1.scope\n";
        // Old escapee on a dead workspace → reaped.
        fake_pid(
            &root,
            "1001",
            Some(cgroup),
            &[("NEEDLE_WORKSPACE", "/home/dead-repo")],
        );
        // Old escapee on a workspace with a live worker → kept (worker pid 1003 below).
        fake_pid(
            &root,
            "1002",
            Some(cgroup),
            &[("NEEDLE_WORKSPACE", "/home/live-repo")],
        );
        // Young escapee on a dead workspace → kept (below the age threshold).
        fake_pid(
            &root,
            "1004",
            Some(cgroup),
            &[("NEEDLE_WORKSPACE", "/home/dead-repo")],
        );

        // A live worker on /home/live-repo. Its "binary" must be named needle*.
        let live_worker = fake_pid(&root, "1003", None, &[]);
        std::fs::write(
            live_worker.join("cmdline"),
            "needle\0run\0--workspace\0/home/live-repo\0",
        )
        .unwrap();
        std::fs::write(root.join("needle"), "#!/bin/sh\n").unwrap();
        let _ = std::os::unix::fs::symlink(root.join("needle"), live_worker.join("exe"));

        // Youngness comes from starttime ticks: 100_000s uptime − 99_999.5s ≈ 0s old.
        std::fs::write(root.join("1004").join("stat"), fake_stat("1004", 9_999_950)).unwrap();

        let reaper = OrphanReaper::with_proc_root(&root);
        let (victims, report) = sweep_orphans(&reaper, 3600, Path::new(&root), Duration::ZERO);
        assert_eq!(victims.len(), 1, "{victims:?}");
        assert_eq!(victims[0].pid, 1001);
        assert_eq!(report.terminated, vec![1001]);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unattributed_scope_processes_are_never_victims() {
        let root =
            std::env::temp_dir().join(format!("needle-orphan-unattr-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        write_uptime(&root, 100_000.0);

        // Old scope-rooted process with no markers and no cwd: not provably a
        // worker-spawned process, so the sweep leaves it.
        fake_pid(&root, "2001", Some("0::/user.slice/run-p1-i1.scope\n"), &[]);

        let reaper = OrphanReaper::with_proc_root(&root);
        let (victims, _) = sweep_orphans(&reaper, 0, Path::new(&root), Duration::ZERO);
        assert!(
            victims.is_empty(),
            "unattributed processes must not be reaped"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn workspace_from_cmdline_handles_both_argument_forms() {
        assert_eq!(
            workspace_from_cmdline("needle run --workspace /x --agent a"),
            Some(PathBuf::from("/x"))
        );
        assert_eq!(
            workspace_from_cmdline("needle run --workspace=/x"),
            Some(PathBuf::from("/x"))
        );
        assert_eq!(workspace_from_cmdline("needle run --resume"), None);
    }

    #[test]
    fn dispatch_ids_are_unique_enough_and_carry_the_worker() {
        let a = generate_dispatch_id("glm-needle");
        let b = generate_dispatch_id("glm-needle");
        assert!(a.starts_with("glm-needle-"));
        assert_ne!(a, b, "two dispatches in the same millisecond must still differ is not required, but same-ms collision would mis-scope a reap");
        assert_ne!(a, generate_dispatch_id("other-worker"));
    }
}
