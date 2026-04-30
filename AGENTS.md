# AGENTS.md

This file contains guidance for AI agents (Claude, GPT, etc.) that are reading the NEEDLE codebase to perform work.

## Telemetry Contract

NEEDLE emits OpenTelemetry-compatible telemetry via OTLP. When modifying code that interacts with the telemetry system, maintain these semantic conventions:

### GenAI Attributes

The `agent.dispatch` span uses OpenTelemetry's `gen_ai.*` semantic conventions:

| Attribute | Description |
|-----------|-------------|
| `gen_ai.system` | AI provider (e.g., `anthropic`, `openai`) |
| `gen_ai.request.model` | Model identifier (e.g., `claude-sonnet-4-6`) |
| `gen_ai.usage.input_tokens` | Input token count |
| `gen_ai.usage.output_tokens` | Output token count |

These attributes enable NEEDLE telemetry to integrate with GenAI-focused dashboards (Grafana GenAI app, Langfuse, Honeycomb AI, etc.).

### Resource Attributes

Every exported signal carries these resource attributes:

| Attribute | Value |
|-----------|-------|
| `service.name` | `"needle"` |
| `service.version` | Build version from `CARGO_PKG_VERSION` |
| `service.instance.id` | Worker ID (e.g., `needle-claude-anthropic-sonnet-alpha`) |
| `needle.session_id` | Per-boot random session ID |
| `host.name` | Hostname |
| `process.pid` | Worker PID |

For the complete semantic mapping of NEEDLE events to OpenTelemetry signals, see [`docs/plan/plan.md`](docs/plan/plan.md).
