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
