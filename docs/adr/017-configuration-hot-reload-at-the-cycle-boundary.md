# ADR-017: Configuration Hot-Reload at the Cycle Boundary

**Status:** Proposed — 2026-08-21
**Deciders:** operator (jedarden)
**Tracking:** plan.md §18; motivated by the 2026-08-21 fleet-wide OTLP migration

## Context

On 2026-08-21 the ex44 fleet was migrated to OTLP export. The change that had
to take effect was a single boolean — `telemetry.otlp_sink.enabled` in
`~/.config/needle/config.yaml`. Applying it required draining and relaunching
**all fifteen workers**, a rollover that took ~55 minutes of wall clock and was
bounded not by the work but by however long each worker's in-flight agent
dispatch happened to run. Every drained worker released its in-flight bead.

Nothing in NEEDLE can pick up a configuration change in place. `Config` says so
in its own doc comment — "Loaded once at boot, immutable during a session" — and
the runtime is built to match:

- `Worker::new` constructs `Telemetry::from_config`, `StrandRunner::from_config`,
  `PromptBuilder::new`, `Dispatcher::new`, `OutcomeHandler::new`,
  `HealthMonitor::new`, and `RateLimiter::new` **once**, each from a snapshot of
  the config, and holds them for the process lifetime.
- `init_tracing_subscriber` (`cli/mod.rs`) installs the OTLP tracing layer with
  `tracing_subscriber::registry()....try_init()` — a **process-global, one-shot**
  install. There is no second chance at it, by design of the tracing crate.
- The remaining config lives in `self.config: Config`, an owned copy read live on
  many paths (`worker.idle_action`, `worker.max_claim_retries`, `agent.default`,
  `workspace.default`, …). These would already follow a swapped value.

Three properties of the 2026-08-21 incident are what this ADR is actually
responding to:

1. **The cost of a config change is a fleet rollover.** Not a restart of one
   process — a staged drain of every worker, each releasing a claimed bead.
2. **A config flip is *armed*, not inert.** needle ≥0.4 fails closed when
   `otlp_sink.enabled` is true and `NEEDLE_OTLP_AUTHORIZATION` is absent:
   `init_tracing_subscriber` → `build_http_providers()?` propagates and the
   worker refuses to boot. Editing the file while a header-less fleet runs does
   not do nothing — it means the *next* restart of each worker kills it.
3. **Config that cannot apply is silently discarded.** `telemetry` sits in
   `NON_OVERRIDABLE_KEYS`, so `telemetry.otlp_sink.enabled: true` in a
   workspace `.needle.yaml` is parsed, warned to a log nobody reads, and
   dropped. The fleet ran for days with a config file that looked correct.

The seam this needs already exists and is already proven. `check_hot_reload()`
runs after the `LOGGING` state, described in-code as running "between dispatch
cycles, never mid-claim, ensuring no bead is left in_progress." Binary
hot-reload has used that boundary safely for months. Configuration reload is the
same problem with a smaller blast radius.

## Decision

### 1. Reload is evaluated only at the existing cycle boundary

The reload check runs where `check_hot_reload()` runs — after `do_log()`,
between dispatch cycles, never mid-claim and never mid-dispatch. A worker in
`Building`/`Dispatching`/`Executing`/`Handling` holds a bead; nothing about its
configuration may change underneath it.

**Rejected:** applying a reload the moment it is detected. It would let
`agent.timeout` or an adapter definition change while a dispatch that was
launched under the old value is still running, producing outcomes attributable
to neither config.

### 2. The trigger is a polled mtime+hash check, interval-gated

Gated by a new `worker.config_reload_check_interval_secs` (`0` disables, and is
the default until the feature has fleet time). This mirrors `check_hot_reload`'s
existing binary-hash check exactly, and adds **no new dependency**.

**Rejected — SIGHUP.** SIGHUP is already bound to the shutdown flag:
`install_unix_signal_handlers` registers `SIGTERM`, `SIGINT`, and `SIGHUP`
together, deliberately, because a killed tmux session delivers SIGHUP to the
worker and the handler exists so the worker can release its bead and emit
`worker.stopped` instead of dying silently. Repurposing SIGHUP would turn every
tmux teardown into a reload. This constraint is not obvious from the outside and
is the single most likely wrong turn for an implementer here.

**Rejected — a file watcher (`notify`).** A new dependency and a new async task,
and it still cannot apply anything at the instant it fires; it would have to
defer to the cycle boundary regardless. The poll is strictly simpler for the
same delivered behaviour.

### 3. Every config key has a declared reload tier

The tier is declared in code as a table, not inferred:

| Tier | Meaning | Examples |
|---|---|---|
| **A — live** | Swap `self.config`; effective next cycle, no rebuild. | `worker.idle_*`, `worker.max_claim_retries`, `agent.timeout`, `budget.*`, strand thresholds |
| **B — rebuild** | Component reconstructed from the new config at the boundary. | `telemetry.*` sinks, `strands.*`, `prompt.*`, `agent.adapters_dir`, `limits.*`, gates/verification |
| **C — immutable** | Requires a restart. | worker identity / `qualified_id`, `workspace.home`, `bead_cli.backend`, the tokio runtime, the shape of the tracing subscriber stack |

**Rejected:** a single atomic swap of the whole `Config` with no tiering. It
reads as if everything is reloadable, and would silently no-op for the Tier-C
keys — recreating the exact `NON_OVERRIDABLE_KEYS` failure that caused this
work.

### 4. Validate before swap; a reload may never fail closed

The candidate config is validated with the existing `ConfigLoader::validate`
before anything is swapped. On any error the running config is kept, a
`config.reload.rejected` event is emitted with the errors, and the worker
continues on its current configuration.

This is non-negotiable and is the direct lesson of the OTLP header: a
configuration problem must degrade telemetry, never remove a worker. A typo in a
YAML file must not be able to take down the fleet — and with polled reload, a
bad edit would otherwise reach every worker simultaneously within one interval,
which is *worse* than the restart-only status quo.

### 5. The reload seam is installed unconditionally at boot

The OTLP tracing layer is wrapped in `tracing_subscriber::reload::Layer` from
the first moment of process start — **including when OTLP is disabled**, where
it wraps a no-op layer.

This is the load-bearing decision. `try_init()` is one-shot: a process that
booted without a reload handle can never acquire one. If the seam is installed
only when OTLP is already enabled, then turning OTLP *on* still requires a
restart — which is precisely the situation this ADR exists to eliminate, and it
would not be discovered until someone tried it on a live fleet. `reload` is not
feature-gated in the pinned `tracing-subscriber` 0.3.23, so this costs no new
dependency.

### 6. Changes that cannot be applied are reported, never silently ignored

A Tier-C key changing produces a `config.reload.restart_required` event naming
the keys, and a WARN. The same applies to a workspace `.needle.yaml` carrying a
`NON_OVERRIDABLE_KEYS` section. Silence is what let the fleet run for days on a
config file that read as correct.

### 7. Secrets are re-resolved, never re-logged, and never tear down a live exporter

`env:`-prefixed header values are re-resolved from the process environment on a
Tier-B telemetry rebuild. A reload **cannot** introduce a new environment
variable into a running process, so if a rebuild would need a header the process
does not have, the existing exporter is kept and the condition reported. A
reload must never convert a working exporter into a dead one. Header values are
never written to a log, an event, or an error message.

## Consequences

- Enabling or disabling OTLP, changing timeouts, thresholds, gates, or prompts
  becomes a file edit plus at most one check interval — no drain, no released
  beads, no rollover.
- A restart remains required for identity, `workspace.home`, bead backend, and
  runtime-shaped concerns. That set is now explicit rather than discovered.
- Tier-B rebuilds add bounded latency at the cycle boundary. Mitigated by
  hashing per config section and rebuilding only components whose own subtree
  changed.
- The blast radius of a bad config edit grows from "the next worker to restart"
  to "every worker within one interval". Decision 4 is what makes that safe, and
  is the reason validation is a hard gate rather than a warning.
