# Changelog

All notable changes to NEEDLE are documented in this file.

## [0.2.6] - 2026-05-16

### Added

- **claude-interactive plugin** (`plugins/claude-interactive/`) — PTY wrapper that runs the Claude Code CLI in interactive mode, keeping workers on subscription billing instead of programmatic API credits. Ships as release assets: `claude-interactive`, `claude-interactive.yaml`, `claude-interactive-install.sh`.

### Fixed

- **Pluck template** — `br close` command no longer passes a `--body` flag (not a valid option); uses default close reason instead.
- **CI deadline** — raised `activeDeadlineSeconds` from 3600 to 7200 to accommodate the full test suite runtime.
- **Process-group kill test** — replaced a fixed 300ms post-SIGKILL wait with a 3-second polling loop so the test passes reliably in container CI environments.

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
