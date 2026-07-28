# Resource Awareness and Worker Density

## Overview

Question investigated (2026-07-27): can NEEDLE throttle its own loop under
memory/CPU pressure, and what would let more workers be packed into the same
box? Findings below come from reading `src/worker/mod.rs`,
`src/rate_limit/mod.rs`, `src/config/mod.rs`, and comparing against live state
on lab (100.81.129.38, 12-worker GLM-4.7 fleet).

**Bottom line:** the coordination primitives needed for both goals already
exist and are already proven safe (used today for RPM/concurrency limiting).
Nothing here requires new cross-process synchronization — it's a matter of
wiring an existing no-op check into the existing decision path, and turning on
a config feature that ships fully implemented but unused.

## 1. CPU/memory awareness exists today, but is a no-op

`RateLimiter::check_system_resources` (`src/rate_limit/mod.rs:350-409`) reads
`/proc/loadavg` and `/proc/meminfo`, compares against
`worker.cpu_load_warn` / `worker.memory_free_warn_mb`, and on breach only:

- logs a `tracing::warn!`
- emits `EventKind::FleetCpuSaturated` / `EventKind::FleetMemoryLow` telemetry

It does **not** affect dispatch. Its own doc comment says as much: "does not
block dispatch." The call site (`src/worker/mod.rs:1814`) is the tell: it runs
*after* `self.rate_limiter.check(...)` has already returned `Allowed` and the
worker is one line away from `set_state(WorkerState::Executing)`. The result
of `check_system_resources` is discarded (`fn` returns `()`).

## 2. The backoff mechanism it should feed into already exists

Immediately above that no-op call, the real gate looks like this
(`src/worker/mod.rs:1787-1806`):

```rust
let decision = self.rate_limiter.check(provider, model, &self.registry)?;
if !decision.is_allowed() {
    // emit RateLimitWait telemetry
    tokio::time::sleep(Duration::from_secs(5)).await;
    return Ok(()); // stays in Dispatching state, retries next loop
}
```

`RateLimitDecision` (`src/rate_limit/mod.rs:30-50`) is an enum:
`Allowed | ProviderConcurrencyExceeded | ModelConcurrencyExceeded |
RpmExceeded`. Adding a `SystemResourcesExceeded { .. }` variant and having
`check_system_resources` return a `RateLimitDecision` instead of `()` — called
from inside `RateLimiter::check()` alongside the other checks — would put
memory/CPU pressure through the *identical* sleep-and-retry loop already
handling RPM backoff. No new locking, no new shared state, no new
thundering-herd risk: it reuses a path already exercised across a
multi-process fleet today.

## 3. Concurrency capping is fully implemented and configured empty everywhere

`LimitsConfig` / `ProviderLimits` / `ModelLimits` (`src/config/mod.rs:1497-1523`)
support `max_concurrent` at both provider and model granularity. The check
(`check_provider_concurrency` / `check_model_concurrency`,
`src/rate_limit/mod.rs:209-255`) counts *actually active* workers via the
shared `Registry` — this is already cross-process safe, it's the same
registry `needle status` reads.

Checked on both this box and lab: `needle config` shows

```yaml
limits:
  providers: {}
  models: {}
```

Empty on both. This means today, worker count and execution concurrency are
the same number — `-c N` controls both how many workers roam/claim *and* how
many can be `EXECUTING` (running a full agent subprocess) at once. Setting
e.g. `limits.models.glm-4.7.max_concurrent: 6` decouples these with **zero
code changes**: 20+ cheap roaming workers can coexist while only 6 ever run
an agent subprocess simultaneously, directly bounding the box's peak memory
footprint regardless of fleet size.

## 4. Per-invocation memory ceiling exists on lab, sized as a backstop not a density control

Lab's adapter (`~/.config/needle/adapters/claude-code-glm-4.7.yaml`) wraps
every dispatch in:

```
systemd-run --user --scope -p MemoryMax=12G bash -c '...'
```

(This wrapping is *not* present in this box's own local adapter of the same
name — worth reconciling, since it means the Hetzner box has no per-agent
ceiling at all today.)

Live measurement on lab (2026-07-27, 12-worker fleet, mid-execution):

| Component | Observed RSS/usage | Configured ceiling |
|---|---|---|
| Idle `needle run` worker process | ~500 MB each | none (not gated) |
| Dispatched `claude` agent subprocess | ~230-400 MB | 12 GB (`MemoryMax`) |
| Aggregate user slice | ~18 GB used (12 workers) | 48 GB (`MemoryMax`), 32 GB (`MemoryHigh`) |

Actual per-invocation usage sits at roughly **2-4% of the configured 12 GB
ceiling**. That ceiling is doing its job as a runaway backstop (stop one
process from taking down the whole box) but has no relationship to normal
usage, so it can't be used to reason about safe fleet density today. Tightening
it to something like 3-4 GB would cost nothing in normal operation and let the
aggregate slice cap support more concurrent workers with the same safety
margin. Note also that only `MemoryMax` is set on the systemd-run wrapper —
no `CPUQuota` — so a CPU-heavy runaway (a `cargo build`/`rustc` storm) has no
per-invocation ceiling, only the coarse whole-slice cgroup cap catches it.

**Correction to a prior assumption:** the idle `needle` worker process itself
is not negligible. ~500 MB RSS per idle worker is a real fixed cost that scales
linearly with roaming-worker count, independent of how many are executing —
worth factoring into any "how many roaming workers can this box hold" math,
separately from the execution-concurrency question in §3.

## 5. No jitter anywhere — the existing backoff is fully deterministic

Checked all three retry/backoff paths in the codebase for randomization:

- `rate_limit/mod.rs` dispatch gate: `tokio::time::sleep(Duration::from_secs(5))`
  — flat, no growth, no jitter.
- `claim/mod.rs` claim retry: `retry_backoff_ms * attempts` — linear, but a
  pure deterministic function of the attempt counter and config.
- `supervisor/mod.rs`: `SPAWN_BACKOFF_SECS = 5`, `ERROR_BACKOFF_SECS = 60` —
  fixed constants.

`rand = "0.8"` is declared in `Cargo.toml` but has **zero real call sites** —
every apparent `rand::` match in `src/` is a regex false-positive from type
names ending in `...Strand::` (`PluckStrand::`, `MendStrand::`,
`ExploreStrand::`, etc. — "Strand::" contains the substring "rand::"). The
dependency is unused.

This is a bigger problem for recommendation §6.3 below than it looks. RPM and
concurrency contention (what the current backoff protects today) partly
self-desynchronizes — workers hit the limit at slightly different moments
because bead claims race individually. CPU/memory pressure, by contrast, is a
**globally correlated signal**: every worker reads the same `/proc/loadavg` /
`/proc/meminfo` at roughly the same instant. Wiring `check_system_resources`
into the existing flat-5s retry as originally proposed would make workers back
off in lockstep and retry in lockstep — a worse, more perfectly synchronized
herd than anything RPM limiting produces today. Any resource-aware gate needs
its own jitter to be safe, it can't just borrow the existing sleep verbatim.

Standard decentralized fixes, most relevant first:

- **Full jitter**: `sleep = random(0, min(cap, base * 2^attempt))` — AWS's
  Architecture Blog found this outperforms both plain exponential backoff and
  "equal jitter" for exactly this kind of correlated-wakeup problem. Purely
  per-node, no coordinator needed.
- **Proactive probabilistic self-throttling** (closer to "as the limit is
  approached" than react-after-rejection): each worker computes
  `p_skip = clamp((load - soft_threshold) / (hard_threshold - soft_threshold), 0, 1)`
  and randomly skips its own claim with that probability. Backoff pressure
  rises smoothly per worker as the shared resource gets hotter, with no
  synchronized cliff where every worker reacts at the same instant. This is
  the RED/CoDel queue-management pattern applied to a worker pool.
- **Decorrelated jitter** (`sleep = min(cap, random(base, prev_sleep * 3))`)
  if full jitter still shows visible re-clustering in practice.
- **Randomized half-open recovery**: when load drops back under threshold,
  backed-off workers should re-probe with independent jittered delay rather
  than all resuming full-rate claiming at once.

## Recommendations, ranked by effort

1. **Set `limits.models.<model>.max_concurrent`** in `.needle.yaml` on any
   fleet where roaming-worker count and desired execution concurrency should
   differ. Zero code changes, already fully implemented (§3).
2. **Tighten `systemd-run -p MemoryMax=` on adapter templates** (and add
   `-p CPUQuota=` alongside it) to something closer to observed usage rather
   than a 12 GB backstop; replicate the wrapper onto adapters that don't have
   it yet (§4).
3. **Wire `check_system_resources` into `RateLimitDecision` — with jitter,
   not the existing flat sleep** — so CPU/memory pressure actually backs off
   dispatch instead of only logging (§1-2, §5). This is a real code change —
   needs a bead, goes through the normal `needle-ci` CI flow per this repo's
   CLAUDE.md. The jitter requirement means this shouldn't just reuse
   `RateLimitDecision`'s existing sleep call unmodified; it needs its own
   randomized delay (§5), and ideally the probabilistic self-throttle rather
   than a hard threshold.

## Source Evidence

- `src/rate_limit/mod.rs` (whole file, esp. lines 1-410)
- `src/worker/mod.rs:1770-1822` (dispatch gating call site)
- `src/config/mod.rs:1497-1523` (`LimitsConfig`/`ProviderLimits`/`ModelLimits`)
- `needle config` output, this box and lab (100.81.129.38), 2026-07-27
- `~/.config/needle/adapters/claude-code-glm-4.7.yaml` on lab vs. this box
- Live `ps`/`systemctl --user status run-*.scope` on lab, 2026-07-27 ~14:30 EDT
- `Cargo.toml` (`rand = "0.8"`) vs. `grep -rn "rand::" src/` (false positives
  only) confirming no jitter exists anywhere in the codebase
