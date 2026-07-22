# ADR-008: Fleet Resource Safety — Enforced CPU/RAM Gating on Worker Launch

**Status:** Proposed — 2026-07-21
**Deciders:** operator (jedarden), via Claude Code
**Tracking:** plan.md Phase 12

## Context

Found during a plan.md maturity review, cross-checking a previously-diagnosed operational incident against the current implementation. Plan.md already states the design intent (`docs/plan/plan.md:115`, `:1699-1718`): "Fleet sizing is bounded by three runtime factors: provider inference throughput, available CPU, and available RAM. NEEDLE monitors these and warns when saturated." The implementation matches the word "warns" literally and does not go further.

**What exists:** `RateLimiter::check_system_resources` (`src/rate_limit/mod.rs:350-400`) reads `/proc/loadavg` and `/proc/meminfo` and emits an `EventKind::FleetCpuSaturated` telemetry event plus a `tracing::warn!` when thresholds are crossed. Its return value is not consulted by any caller to gate or delay anything — it is a pure side-effecting observability call.

**Where it's called:** only from `do_dispatch()` (`src/worker/mod.rs:1607-1612`) — inside an **already-running** worker's loop, immediately before executing a bead that worker has **already claimed**. It is never called during `worker_construction` (`src/cli/mod.rs:861-868`, a plain sequential `init_step(...)` call with no CPU/RAM gate before or during it), and never called during the `--count=N` sequential-launch sequence.

**Launch spacing is a fixed interval, not load-adaptive:** `src/cli/mod.rs:637-638` — `if seq > 0 && stagger_secs > 0 { std::thread::sleep(Duration::from_secs(stagger_secs)); }`, where `stagger_secs = config.worker.launch_stagger_seconds` defaults to `2` (`docs/plan/plan.md:1739`). This delay is the same whether the box is idle or already at 3x its core count in load average.

**The gap this leaves, previously diagnosed operationally (2026-07-19, lab):** `worker_construction` is a genuinely slow (~5s) init step — it builds the agent adapter and warms the provider proxy connection. Under CPU saturation it can overrun whatever margin it has and the OS terminates the process before it completes — no Rust panic, no backtrace, just a worker that silently vanishes seconds after being launched. This was diagnosed by direct comparison: an identical launch command died twice at load ~2.5, then succeeded 90 minutes later at load ~0.74 with `worker_construction completed in 4992ms` — just under whatever margin exists. (The precise OS-level kill mechanism — cgroup pressure, systemd deadline, scheduler starvation past some other bound — was not re-diagnosed as part of this codebase review; what *is* newly confirmed by this review is that NEEDLE's own code has no gate that would prevent launching into this condition in the first place.) NEEDLE's own 60-second boot timeout (`src/cli/mod.rs:870-875`) cannot catch this class of failure, since it only evaluates *after* `init_step` returns — a process killed mid-step by something outside NEEDLE never reaches that check.

Net effect: the one place in the codebase that actually reads system load (`check_system_resources`) is checked at the wrong point in the lifecycle to prevent the failure mode plan.md already anticipated in prose. It protects an already-running worker from starting *another* bead under saturation; it does nothing to stop a *new* worker from being launched into saturation in the first place, or to slow down a batch launch that is itself the cause of the saturation.

## Decision

1. **12.1 — Gate `worker_construction` on system resources, not just `do_dispatch`.** Before entering `worker_construction` (`src/cli/mod.rs`, around line 861), call `check_system_resources()` (or a renamed/generalized variant, since it's no longer rate-limit-specific once used here). If saturated (same thresholds already defined for `FleetCpuSaturated`), do not proceed into the slow construction step — retry with backoff (a few seconds, capped) until load drops below threshold or a max-wait is hit, at which point fail the launch with a clear, actionable error (`"deferred N times, system still saturated (load X vs threshold Y) — launch aborted, retry when load drops"`) rather than letting the OS silently kill an in-flight construction. This turns an unexplained vanished-worker into an explicit, loggable, retryable outcome.
2. **12.2 — Load-adaptive launch staggering.** Replace the fixed `launch_stagger_seconds` sleep in the `--count=N` sequential-launch path with a check against current load before each subsequent launch: if load is below a comfortable threshold, use the existing (short) default delay; if above it, extend the wait (bounded, with a cap) until load recedes or the cap is hit. This directly targets the failure mode where the *stagger itself* is what's supposed to prevent thundering-herd saturation but currently does so blindly regardless of whether the previous launches already pushed load past a safe point.
3. **12.3 — Apply the same gate to `needle supervise`'s auto-scale spawn path**, not only the CLI's `--count=N` path — the supervisor daemon (`src/supervisor/mod.rs`) is the other place new workers get launched, and it should not spawn a worker into saturation any more than a manual `--count=N` invocation should.
4. **Ship through the existing release path**: version bump, needle-ci (fmt + clippy + test on iad-ci), GitHub Release, staged canary rollout (`:testing` → `:stable`).

## Consequences

- Positive: closes the gap between plan.md's stated design intent ("NEEDLE monitors these and warns when saturated") and its actual behavior (monitors and warns only for an already-running worker's next-bead decision) — the fix is a matter of calling an existing function from two more call sites and consulting its result, not building new infrastructure.
- A deferred/retried launch under 12.1 changes `needle run`'s observable behavior under load — a launch that previously either silently died or (rarely) squeaked through now explicitly waits or explicitly fails with a reason. Any external tooling (scripts, the fleet launcher wrapper) that assumes `needle run` returns immediately after backgrounding into tmux needs to tolerate a longer, bounded wait under saturation.
- 12.2's adaptive stagger means a large `--count=N` batch launch on an already-busy host will take longer wall-clock time than before — this is the intended tradeoff (slower but reliable launches vs. fast but silently-lossy ones), consistent with the existing operational practice of manually staggering launches while watching `uptime`.
- Does not address the separate, harder question of *how many workers is too many* for a given host long-term (that's the "ready-bead supply is the real ceiling, not hardware" finding from a separate operational audit) — this ADR is scoped to preventing a single launch from being silently killed mid-construction, not to fleet-sizing policy in general.

## Evidence

- `docs/plan/plan.md:115`, `:1699-1718` (stated design intent: "NEEDLE monitors these and warns when saturated").
- `src/rate_limit/mod.rs:350-400` (`check_system_resources`, reads `/proc/loadavg`/`/proc/meminfo`, emits `FleetCpuSaturated`, return value unused by callers).
- `src/worker/mod.rs:1607-1612` (only call site: `do_dispatch()`, already-running worker, already-claimed bead).
- `src/cli/mod.rs:861-868` (`worker_construction` init step, no resource gate before/during); `:870-875` (60s boot timeout, evaluated only after `init_step` returns — structurally cannot catch a mid-step external kill).
- `src/cli/mod.rs:637-638` (fixed-interval launch stagger); `docs/plan/plan.md:1739` (`launch_stagger_seconds` default `2`).
- 2026-07-19 lab diagnosis: identical `needle run -w /home/coding/commitgraph -a claude-code-glm-5 -c 1 -i X` died twice at load ~2.5 (stderr ends mid-`worker_construction`, no panic/backtrace), succeeded at load ~0.74 (`worker_construction completed in 4992ms`) — load was the only variable that changed between failed and successful attempts.
