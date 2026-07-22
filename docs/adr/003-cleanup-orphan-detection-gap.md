# ADR-003: Cleanup Command Orphan-Detection Gap

**Status:** Accepted — 2026-07-19
**Deciders:** operator (jedarden), via Claude Code
**Tracking:** plan.md Phase 7; implementation beads in this repo's workspace (genesis TBD — see plan.md Phase 7)

## Context

Found during lab fleet remediation on 2026-07-19. Claude Code, acting on the operator's behalf, was auditing fleet productivity: worker `armor-p6a` (pinned to `~/ARMOR`) was confirmed genuinely productive; four workers pinned to `~/commitgraph` (`alpha`, `bravo`, `charlie`, `delta`) were either legitimately exhausted (no ready work) or burning cycles in an operator-gated re-verify loop; a roaming worker (`cgov`) was found stuck failing an orphaned-bead release call every ~17-minute idle cycle. The remediation plan was to stop the unproductive workers and relaunch fresh roaming ones.

Two things happened that didn't match the plan:

1. `needle stop -i alpha` and `needle stop -i delta` each reported `Stopped: needle-claude-code-glm-5-<name>`, but `ps aux` continued to show both underlying `needle run --workspace ~/commitgraph` processes running (PIDs 2244216 and 2248754) after the stop. This reproduces the exact defect plan.md §6.2 / ADR-002 "Bug 2" already describes and has planned — but not yet implemented — a fix for.
2. A subsequent bare `needle cleanup` (no `--all`, no `-i`, intended only to remove the now-orphaned tmux sessions for the workers just stopped) instead reported `Cleaned up: needle-claude-code-glm-5-armor-p6a` and `Cleaned up: needle-supervisor`. Neither was orphaned: `armor-p6a` had a live process actively executing (170+ accumulated CPU-hours at the time), and `needle-supervisor` was the fleet's own auto-scaling daemon. Both tmux sessions were destroyed. tmux's server process then exited on its own — `tmux list-sessions` reported `no server running on /tmp/tmux-1001/default` — because tmux auto-terminates once its last session is gone. This read initially like a wider, unexplained incident; it was fully explained once the actual `cleanup` implementation was read.

Source inspection (`src/cli/mod.rs:1435-1487`) found the root cause. `cmd_cleanup`'s own help text (`"Remove orphaned tmux sessions (sessions without active workers)"`) and doc comment (`"Finds and removes needle tmux sessions that no longer have active workers."`) both promise a liveness check. The implementation performs none:

```rust
let targets: Vec<&str> = if all {
    sessions.iter().map(|s| s.name.as_str()).collect()
} else {
    let id = identifier.as_deref().unwrap_or("");
    sessions
        .iter()
        .filter(|s| id.is_empty() || s.name.contains(id))
        .map(|s| s.name.as_str())
        .collect()
};
```

When called with neither `--all` nor `-i`, `identifier` is `None`, so `id` is `""`, so `id.is_empty()` is always `true`, so every session matches the filter regardless of name. **Bare `needle cleanup` is functionally identical to `needle cleanup --all`.** No code path in `cmd_cleanup` calls `scan_needle_processes()` — the exact process-table reconciliation helper `cmd_list` already uses, and the one §6.2's exit criteria commits to building out for `needle status`/`needle list` — to check whether a session's registered PID actually corresponds to a live process. The command's own documentation describes a safety property that does not exist in the code.

Net effect: `needle cleanup` is a hidden `--all` with extra steps whenever a caller omits `-i`, and its output (`Cleaned up: <name>`) is indistinguishable from a genuinely safe, scoped cleanup — there is no signal at the call site that anything destructive just happened to a live process.

## Decision

1. **Implement real liveness-based orphan detection as cleanup's default (no-flags) behavior.** A tmux session is a target only if `scan_needle_processes()` finds no live process backing its registered PID. Reuse this existing reconciliation helper rather than building a second, divergent liveness check — it is the same primitive §6.2 already commits to for `needle status`/`needle list`, so this ADR's fix and §6.2's fix should share one implementation once both land.
2. **`--all` keeps its current fully-destructive meaning, unchanged.** Its own `--help` text is updated to say so plainly ("removes every needle session, including live ones") so the danger is visible at the call site, not only in this design doc.
3. **`-i <pattern>` keeps its current targeted, deliberate meaning** — naming a specific session by pattern is itself the operator's explicit choice, so it continues to bypass the liveness check exactly as today. Only the no-flags path's behavior changes.
4. **Update `cmd_cleanup`'s help text and doc comment** to describe the behavior that will now actually exist, and add regression tests pinning all three paths (no-flags-live-preserved, no-flags-all-dead-noop, `--all`-unchanged) so the docs-vs-implementation gap that caused this incident cannot silently reopen.
5. **Ship through the existing release path**: version bump, needle-ci (fmt + clippy + test on iad-ci), GitHub Release, staged canary rollout (`:testing` → `:stable`) — per plan.md's established Deployment convention (§6.8).

## Consequences

- Bare `needle cleanup` becomes safe to run without a manual `ps aux` cross-check first — matches the same trust bar §6.2/ADR-002 already set for `needle stop`/`status`/`list`.
- No behavior change for `--all` (still fully destructive, now honestly documented) or `-i <name>` (still targeted/deliberate, unchanged).
- Cleanup's no-flags path now shells out to the same process-scan `cmd_list` already performs, rather than a single `tmux kill-session` loop over every registered name — bounded cost, only paid when cleanup is actually invoked.
- This fix's implementation depends on `scan_needle_processes()`-based reconciliation; if §6.2's landing slips, this ADR should ship its own minimal PID-liveness check rather than block on it, to avoid leaving the no-flags footgun live in the meantime.
- Does **not** fix the separate, already-tracked §6.2 defect (`needle stop` leaving orphaned processes behind after reporting success) — that remains open, and is in fact what produced the very "orphaned" sessions this bug then over-broadly swept up in the 2026-07-19 incident. The two defects compound: a not-fully-killed `needle stop` leaves a real orphan behind, but an unrelated live session sitting next to it in the tmux session list gets caught in the same net.
- `needle-supervisor` has no automatic restart-on-crash of its own; recovering from this incident required a fully manual relaunch. Worth a follow-up, not scoped to this ADR.

## Evidence

- `src/cli/mod.rs:1435-1487`, `cmd_cleanup` — full function reproduced above under Context; confirmed no liveness check exists in the `all == false` branch, and `sessions.iter().filter(|s| id.is_empty() || ...)` unconditionally matches everything when no `-i` is given.
- 2026-07-19 lab incident: `needle stop -i alpha` and `needle stop -i delta` both reported `Stopped:` while `ps aux` continued to show `needle run --workspace /home/coding/commitgraph --identifier alpha` (PID 2244216) and the equivalent `delta` process (PID 2248754) alive, post-stop.
- 2026-07-19 lab incident: bare `needle cleanup` output — `Cleaned up: needle-claude-code-glm-5-armor-p6a` and `Cleaned up: needle-supervisor`, immediately followed by `tmux list-sessions` → `no server running on /tmp/tmux-1001/default`. `armor-p6a`'s underlying process (PID 1421255, 170+ accumulated CPU-hours) survived independently of its tmux session and continued executing; `needle-supervisor`'s process did not survive and required manual relaunch.
- Related, already-tracked defect reproduced (not newly discovered) in the same incident: plan.md §6.2 / ADR-002 "Bug 2" (`needle stop` not killing the full process tree) — this ADR's Context is fresh, concrete evidence for that still-open item, not a separate finding.

## Addendum (2026-07-21): the shipped fix (bf-1ep0s / commit b5ada58) is itself broken

Found during a plan.md maturity review, before this ADR's Decision had been re-verified against the merged code. `cmd_cleanup`'s new no-flags path (`src/cli/mod.rs`, the `else` branch added in b5ada58) does:

```rust
let discovered = scan_needle_processes().unwrap_or_default();
let live_pids: std::collections::HashSet<u32> = discovered.iter().map(|p| p.pid).collect();
sessions
    .iter()
    .filter(|s| s.pid.map_or(true, |pid| !live_pids.contains(&pid)))
```

`s.pid` comes from `TmuxSession.pid`, populated from tmux's `#{pane_pid}` (`list_needle_sessions`, `src/cli/mod.rs:4046,4086`) — the PID of the pane's shell, not the needle binary. `scan_needle_processes()` (`src/cli/mod.rs:4137-4261`) *deliberately excludes* that same shell PID: its own inline comment reads "IMPORTANT: Exclude shell wrapper processes ... We only want to discover the actual needle worker process, not the shell wrapper" (lines 4186-4188, 4196-4205), and it returns only the PID of the exec'd `needle run` process itself.

Whether these two PIDs actually differ depends on whether tmux/bash exec-replaces the shell with the needle binary or forks a child for it — this was verified empirically rather than assumed, by reproducing NEEDLE's exact launch invocation:

```
$ tmux new-session -d -s needle-pidtest "NEEDLE_INNER=1 sleep 30 2>> /tmp/needle-pidtest.log"
$ tmux list-panes -t needle-pidtest -F '#{pane_pid}'
3322398
$ pstree -p 3322398
bash(3322398)---sleep(3322399)
```

`pane_pid` is the `bash -c "NEEDLE_INNER=1 ... 2>> logfile"` wrapper (the output redirection defeats bash's last-command exec optimization); the actual worker process is a **child** with a different PID. This exactly mirrors `launch_in_tmux()`'s real invocation shape (`src/cli/mod.rs:955-971`: `NEEDLE_INNER=1 {self_exe} {args} 2>> {stderr_log}`).

**Consequence:** `s.pid` (always the shell wrapper's PID) can never appear in `live_pids` (which structurally excludes shell wrappers) for *any* tmux-launched session — the liveness check's `!live_pids.contains(&pid)` is `true` unconditionally. Bare `needle cleanup` still classifies every live tmux-backed session as orphaned; the fix does not reduce the 2026-07-19 incident's blast radius, it just changes which line of code produces the same result. `cmd_stop` already solves exactly this shell-vs-child ambiguity via `find_needle_process_in_tree()` (`src/cli/mod.rs:1198-1213`, walks the descendant tree from `pane_pid` looking for the actual `needle run` process) — `cmd_cleanup`'s liveness check does not call it, or any equivalent tree-walk, at all.

No test caught this because the existing/planned regression tests (ADR-003 Decision #4, plan.md §7.2) test `scan_needle_processes()`'s filtering logic and `cmd_cleanup`'s selection logic against constructed fixtures, not against a real tmux session — the exact indirection that hides the bug. See plan.md Phase 7 §7.1a for the fix and an updated test requirement (an actual tmux-session-based regression test, not a unit-level one).
