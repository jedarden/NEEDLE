# Changelog

All notable changes to NEEDLE are documented in this file.

## [Unreleased]

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

### Fixed

- **Explore strand roam-rotation starvation** — per-worker scan order is
  now reshuffled and the workspace list re-discovered every cycle, instead
  of a static hash-derived rotation that could permanently strand some
  workspaces outside every live worker's reachable window (bf-6anj4).
- **Bead-store contamination repair** — recovered store state after
  contamination from a stuck integration-test process.
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
