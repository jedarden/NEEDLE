# Changelog

All notable changes to NEEDLE are documented in this file.

## [Unreleased]

### Phase 2

#### Added

- **OTLP Sink** - OpenTelemetry telemetry export
  - Export traces, metrics, and logs to any OTLP-compatible backend
  - Supports gRPC and HTTP/protobuf transports
  - Non-blocking batch processor with graceful shutdown
  - Follows OpenTelemetry `gen_ai.*` semantic conventions for LLM telemetry
  - Configure via `.needle.yaml` under `telemetry.otlp_sink`
  - See `docs/examples/otel-collector/` for a working docker-compose example

#### Documentation

- **Observability section** in README.md
  - Overview of exported signals (traces, metrics, logs)
  - Minimal OTLP configuration example
  - Link to semantic mapping in `docs/plan/plan.md`

- **AGENTS.md** - Telemetry contract for AI workers
  - GenAI semantic conventions (`gen_ai.system`, `gen_ai.request.model`, `gen_ai.usage.*`)
  - Resource attributes carried by all exported signals

- **OTLP Collector example** (`docs/examples/otel-collector/`)
  - docker-compose setup with OpenTelemetry Collector, Jaeger, Prometheus, Loki, and Grafana
  - Quick start guide for local testing
  - Config files for collector, Prometheus, and Loki

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
