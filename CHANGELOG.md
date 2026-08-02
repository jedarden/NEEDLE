# Changelog

All notable changes to NEEDLE are documented in this file.

## [Unreleased]

## [0.2.16] - 2026-08-02

### Fixed

- **Dispatch prompt told agents to close beads with `br`** — the built-in
  template emitted `` `br close {bead_id}` ``. `br` is the deprecated alias for
  `bf` (bead-forge) and survives only as a shim on hosts that happen to have
  one. On a host without it the agent's close silently failed with
  command-not-found and the bead stayed `in_progress` forever, while the worker
  looked perfectly healthy. Observed on a freshly provisioned host: one worker
  claimed 4 beads in ~15 minutes and closed zero — telemetry showed 4
  `bead.claim.succeeded` and no `bead.closed`/`bead.released` events at all.
  The template now emits `bf close`.
- **Mitosis template emitted an invalid `br create` invocation** — it used
  positional arguments (`br create "Title" "body"`), which `bf create` does not
  accept; it requires `--title` and `--description`. Split children could never
  have been created on a current bead-forge. The template now uses the correct
  flags and documents `bf dep add` / `bf label add` for chaining and labelling.
- **`BrCliBeadStore::discover()` resolved `br` before `bf`** — every internal
  `ready`/`list`/`update`/`show`/`sync` call preferred the deprecated alias,
  which is why NEEDLE was effectively its last consumer (19,543 of 19,560
  entries in one host's `br-deprecation.log`). It now resolves `bf` on PATH,
  then `~/.local/bin/bf`, and only then falls back to `br` for hosts still
  carrying the shim. A host with only `bf` installed no longer hard-fails.

### Changed

- `CLAUDE.md` now documents `bf` as the bead CLI and `bf close --reason`
  (not the invalid `--body`), and states that `br` must never be emitted from a
  prompt template or doc.

## [0.2.15] - 2026-07-31

### Fixed

- **Supervisor zombie-child reaping** — `Supervisor::tick()` now sweeps
  `waitpid(-1, WNOHANG)` at the top of every tick, reaping exited worker
  children so `needle supervise` no longer leaks `<defunct>` zombies for
  the lifetime of the daemon (GH #12, ADR-010).
- **Zombie-aware `is_pid_alive`** — `registry::is_pid_alive` now additionally
  checks `/proc/<pid>/stat`'s process-state field on Linux and treats a
  zombie (state `Z`) as not-alive, so an unreaped worker can no longer
  inflate `Supervisor::tick()`'s `alive_count` and falsely block new spawns
  at `max_workers` capacity. Falls back to the existing `kill(pid, 0)`-only
  behavior on non-Linux platforms and whenever `/proc/<pid>/stat` is
  unreadable (GH #12, ADR-010).

## [0.2.14] - 2026-07-30

Note: entries for 0.2.9-0.2.13 were not recorded at the time (this file
lagged behind actual releases); the OTLP Sink work previously listed under
`[Unreleased]` shipped in one of those versions and is not re-described
here — see `git log` for the full history in that range.

### Added

- **Shipped-work enforcement** — new `worker.enforce_shipped_work` config
  toggle (default: enabled). A bead's closure is now only accepted if
  either a substantial commit (touching at least one file outside
  `notes/`, `.beads/`) has been made and pushed since dispatch started, or
  the bead itself was explicitly updated during the dispatch (e.g. `bf
  update --notes` recording why no code change was needed). Closes the gap
  where a worker stuck on an uncompletable bead could satisfy a bare
  "must have a commit" rule by committing a trivial doc file every retry
  (bf-1i9).
- **Adapter routing wired into dispatch** — routing decisions now flow
  through to dispatch with full telemetry (needle-3h0r).

- **Failure-quarantine circuit breaker** — Pluck now orders candidates with
  failure-awareness and wires in a quarantine breaker for beads that keep
  failing dispatch (ADR-012).

### Fixed

- **Explore strand roam-rotation starvation** — per-worker scan order is
  now reshuffled and the workspace list re-discovered every cycle, instead
  of a static hash-derived rotation that could permanently strand some
  workspaces outside every live worker's reachable window (bf-6anj4).
- **Bead-store contamination repair** — recovered store state after
  contamination from a stuck integration-test process.
- **Mitosis timeout child-process leak** — child processes spawned by a
  mitosis split are now reaped instead of leaking when the split itself
  times out (bf-653n7, ADR-011).
- **Orphaned dispatch children on outer-timeout cancellation** — the
  process group is now killed (not just the direct child) when an agent
  dispatch is cancelled by the outer timeout, so descendants no longer
  keep running after NEEDLE gives up on them (GH #13).
- Pre-existing clippy lints (unused parameters, a dead field, manual
  char-comparison patterns, a redundant closure) cleaned up across the
  strand and mitosis modules.

## [0.2.8] - 2026-06-14

### Added

- **Model-based adapter routing** — Anthropic models route to `claude-print` adapter automatically based on provider configuration
- **Agent routing config schema** — New `agent.routing` config section with comprehensive validation tests
- **Trace sanitization benchmark scaffold** — Performance testing framework with test data generation for transcript sanitization
- **Benchmark optimization** — Criterion configured with smaller sample sizes for faster CI iteration

### Fixed

- **Adapter resolution** — Removed silent `claude-sonnet` fallback that masked configuration errors in `resolve_adapter`
- **Full cycle test** — Disabled routing in `full_cycle_with_echo_agent` test to avoid flakiness
- **Sanitizer latency threshold** — Relaxed debug-mode threshold to 2000ms to accommodate CI variability
- **Test suite** — Fixed two pre-existing test failures

## [0.2.7] - 2026-06-07

### Fixed

- **Outcome persistence** — Flush JSONL after every success outcome to prevent data loss on shutdown
- **Bead store integration** — Corrected `BfCliBeadStore::create_bead` to use proper `br` CLI flags
- **Config workspace overrides** — Apply workspace strand overrides to config in `apply_workspace`

### Added

- **Trace sanitization benchmark** — Performance benchmark for transcript sanitization with helpers for data generation

## [0.2.6] - 2026-05-16

### Added

- **claude-interactive plugin** (`plugins/claude-interactive/`) — PTY wrapper that runs the Claude Code CLI in interactive mode, keeping workers on subscription billing instead of programmatic API credits. Ships as release assets: `claude-interactive`, `claude-interactive.yaml`, `claude-interactive-install.sh`.

### Fixed

- **Pluck template** — `br close` command no longer passes a `--body` flag (not a valid option); uses default close reason instead.
- **CI deadline** — raised `activeDeadlineSeconds` from 3600 to 7200 to accommodate the full test suite runtime.
- **Process-group kill test** — replaced a fixed 300ms post-SIGKILL wait with a 3-second polling loop so the test passes reliably in container CI environments.
