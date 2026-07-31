# ADR-011: Process-Group Kill Guard for Outer-Timeout-Cancelled Agent Dispatch

**Status:** Accepted — 2026-07-31
**Deciders:** operator (jedarden), via Claude Code
**Tracking:** GitHub issue [#13](https://github.com/jedarden/NEEDLE/issues/13); bf-653n7 (bug), bf-nqphu (fix), bf-35usk (test), bf-57n1j (audit), bf-5wa8m (this ADR)

## Context

On 2026-07-30/31, the lab NEEDLE fleet (`100.81.129.38`, 16 pinned/roaming
workers) suffered two full-fleet collapses within about an hour, both with
the same signature: a single worker's process tree grew to 56→146
(`warden-p1`) and separately 0→74 (`claude-print-opus-alpha`) descendant
`claude --disallowed-tools spawn_worker` processes within minutes, driving
load average to 248–308 on a 12-core box. Both times, killing the runaway
tree happened to also kill the shared tmux server (the offending process was
the first `tmux new-session` call after a clean boot, and so ended up hosting
the server), taking all 16 registered workers down with it.

Root-caused via live log tracing on the running fleet, not from a bug report:
`Worker`'s per-bead handling (`src/worker/mod.rs`) runs a "mitosis
evaluation" step after every bead failure — a follow-up agent dispatch that
decides whether to split the bead — wrapped in
`tokio::time::timeout(Duration::from_secs(120), self.mitosis_evaluator.evaluate(...))`.
In `warden-p1`'s log, the same two beads (`wd-1sk`, `wd-3oc`) had this
evaluation time out on 5 of 5 observed attempts over a 2+ hour span. Each
timeout leaked one more orphaned `claude` process — exactly the runaway
signature observed in the incident.

### Why the process leaked

`mitosis::Evaluator::evaluate` (`src/mitosis/mod.rs:183`) calls
`dispatcher.dispatch(...)` (`src/mitosis/mod.rs:343`), which shares the same
`Dispatcher::run_process` (`src/dispatch/mod.rs`) used by the main per-bead
dispatch path. `run_process` spawns the agent via
`tokio::process::Command::new("bash")...spawn()` with `setpgid(0, 0)` in a
`pre_exec` hook, and has its own internal timeout handling
(`adapter.effective_timeout(...)`, independently configurable, commonly
1200s+) that correctly kills the whole process group on fire:
`libc::killpg(pid, SIGKILL)` followed by `child.start_kill()` and
`child.wait()`.

That internal kill logic is correct — and unreachable in this scenario. The
*outer* 120s mitosis-evaluation timeout in `worker/mod.rs` wraps the entire
`evaluate()` call (prompt build, dispatch, response parse), a materially
shorter duration than the agent's own internal timeout. When the outer
timeout fires first, it drops the whole in-flight future — including
`run_process`'s `match tokio::time::timeout(timeout_dur, child.wait()).await`
expression — before that match ever resolves. A dropped Tokio future does
not run the code inside an unreached match arm; it is torn down at whatever
`.await` point it was suspended at. Dropping a future that's mid-`Child::wait()`
does not kill the OS process (no `kill_on_drop(true)` was set on the spawn),
so the child — and, since `bash -c "<agent> ..."` does not `exec`-replace,
its own child, the actual `claude` process — is simply orphaned, reparented,
and left running indefinitely.

This is the same *class* of bug as GitHub issue #12 (`Supervisor::spawn_worker`
never reaping under `needle supervise`, ADR-010) but a distinct instance:
ADR-010 explicitly scopes its fix as not applying to `needle run`
single-worker mode. This bug lives entirely inside `Worker`'s own per-bead
loop and reproduces under plain `needle run` — the mode every pinned/roaming
worker on lab actually runs in. ADR-010's scoping assumption was incomplete,
not wrong about issue #12 itself.

### Audit of other timeout-wrapped spawns (bf-57n1j)

Grepped the crate for `tokio::time::timeout` wrapping anything that reaches a
process spawn:

- **`strand/weave.rs`** (`WEAVE_STRAND_TIMEOUT_SECS`, 120s, wraps
  `evaluate_internal`) — **also affected**, and also a real agent CLI
  invocation (`CliWeaveAgent::analyze_gaps`, `bash -c "<agent> --print < ..."`),
  not a cheap operation. Same root cause, same fix applied.
- **`worker/mod.rs`'s commit-hook timeout** (30s, wraps
  `commit_hook::inject_bead_id_trailer`) and **`commit_hook.rs`'s own internal
  10s/30s timeouts** around `git rev-parse` / `git log` / `git commit --amend`
  — theoretically the same gap (no `kill_on_drop`), but categorically lower
  risk: these are simple, typically sub-second git invocations that
  essentially never approach their already-generous timeouts in practice,
  and an orphaned `git` process is cheap (no LLM API cost, no long-running
  work) compared to an orphaned agent session. Hardened anyway
  (`kill_on_drop(true)`, mechanical and low-risk) since the fix was trivial
  once the pattern was known.
- **`mend`, `explore`, `pluck` strands** — no `tokio::time::timeout` usage at
  all; not applicable.

## Decision

1. **`ProcessGroupKillGuard`** (`src/process_guard.rs`, new module): an RAII
   guard holding a spawned child's PID (which is also its process group ID,
   per the existing `setpgid(0, 0)` convention), armed at construction and
   killing the whole group (`libc::killpg(pid, SIGKILL)`) on `Drop` unless
   `disarm()` was called first. This is deliberately independent of *why* the
   future was dropped — outer timeout, panic, or any future cancellation
   reaches the same `Drop` path. Callers `disarm()` the guard immediately
   after they've already reaped the process through their own normal path
   (successful `wait()`, or their own manual timeout-kill), so the guard is a
   no-op on every happy path and only ever fires SIGKILL in the abnormal
   cancellation case this ADR exists to close.
2. **Wired into `Dispatcher::run_process`** (`src/dispatch/mod.rs`): guard
   constructed right after spawn, disarmed on all three exit paths of the
   existing timeout match (normal wait, wait error, internal timeout after
   its own manual kill). Fixes the mitosis-evaluation leak and, since
   `run_process` is shared, any other caller that wraps `dispatch()` in an
   outer timeout.
3. **Wired into `CliWeaveAgent::analyze_gaps`** (`src/strand/weave.rs`):
   restructured from `Command::output()` (an atomic spawn-and-wait that
   offers no point to insert a guard) to `spawn()` then
   `child.wait_with_output()`, with the same `setpgid(0, 0)` pre_exec hook
   `run_process` uses, so the guard's `killpg` targets only this child's own
   group.
4. **`kill_on_drop(true)`** added to `commit_hook.rs`'s four git spawns —
   simpler than the process-group guard because these commands are not
   expected to fork further children, and `kill_on_drop` only needs to reach
   the single direct child.

## Alternatives Considered

- **Retain `Child` handles and check `try_wait()` per poll** — rejected: would
  require threading owned `Child`/PID state through call sites that don't
  otherwise need it, purely to recover what a `Drop` impl gets for free.
- **`kill_on_drop(true)` alone on the main agent spawn** — rejected as
  insufficient: it only reaches the *direct* child (`bash`), not any process
  it itself forks without `exec`-replacing (the actual `claude` invocation in
  production). The existing internal timeout logic already establishes that
  a process-group-wide `killpg` is necessary here (see its own comment: "kill
  the entire process group so subprocesses spawned by the agent ... are also
  reaped, not just the direct bash child") — a fix that only handles the
  direct child would silently reintroduce the leak for the one-hop-removed
  case, i.e. exactly the case actually observed in production.
- **Shorten or remove the mitosis-evaluation 120s timeout** — rejected as a
  non-fix: it would reduce leak *frequency* by changing when the race is lost,
  not eliminate the race itself, and 120s is already a deliberate,
  documented ("prevent indefinite hang") bound the operator does not want to
  relax.

## Consequences

- The mitosis-evaluation timeout leak (the confirmed, evidenced cause of
  tonight's two fleet collapses) is closed at its source.
- The same class of leak in the weave strand's gap-analysis dispatch is
  closed pre-emptively — confirmed reachable via the same outer-timeout
  pattern, not yet observed in production, but using the identical (real,
  expensive) agent-CLI spawn shape as the mitosis case.
- `commit_hook.rs`'s git spawns are hardened as a low-risk, low-cost
  follow-on; no behavior change on any success or already-handled-timeout
  path.
- No behavior change to any caller on the happy path: the guard's `Drop` is a
  no-op once `disarm()` has run, which happens on every existing
  success/timeout/error branch that was already there.
- Adds one new leaf module (`src/process_guard.rs`) with no new external
  dependencies (`libc` was already a dependency, used the same way, in both
  modified files).

## Evidence

- `src/worker/mod.rs` (~line 2355) — `tokio::time::timeout(Duration::from_secs(120), self.mitosis_evaluator.evaluate(...))`, `Err(_)` branch logs and continues, does not touch the spawned child.
- `src/mitosis/mod.rs:183,343` — `Evaluator::evaluate`, `dispatcher.dispatch(...)` call.
- `src/dispatch/mod.rs` (`run_process`) — spawn at `Command::new("bash")...spawn()` with `setpgid(0, 0)`; internal timeout-and-killpg logic that is correct but unreachable when cancelled from outside.
- `src/strand/weave.rs` (`WEAVE_STRAND_TIMEOUT_SECS = 120`, `CliWeaveAgent::analyze_gaps`) — same outer-timeout-wraps-inner-spawn shape.
- Live evidence, lab `100.81.129.38`, `needle-claude-code-glm-4_7-warden-p1.stderr.log`, 2026-07-30/31: repeated `"mitosis evaluation timed out after 120s, continuing to LOGGING"` for `wd-1sk` (21:16, 21:21, 22:10, 22:27 UTC) and `wd-3oc` (01:33 UTC), 5/5 observed timeout rate.
- Incident process counts: `warden-p1` tree reached 146 descendants; `claude-print-opus-alpha` reached 74; both entirely `claude --disallowed-tools spawn_worker` children. Load average 248–308 on a 12-core box during both collapses.
