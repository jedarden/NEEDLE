# Agent-event fixtures

Upstream JSONL streams, one per shipped output transform, shaped as each
agent CLI actually emits them. The conformance suite in
`tests/integration_spawn/agent_event_schema_conformance.rs` pipes each file
through the matching `needle-transform-*` binary and validates every emitted
line against `docs/agent-event-schema.md`.

Every fixture deliberately exercises, in one stream:

- **all five documented event types** — `tool_call`, `tool_result`,
  `agent_message`, `tokens`, `error` — so the conformance assertions can
  never pass vacuously;
- the adapter's **skipped/lifecycle** line types (`system`, `step_start`,
  `turn_end`, `message_update`, …);
- one **malformed** (`MALFORMED_SENTINEL`) line, one missing `type`, and one
  `needle_future_unknown` type — the transform must skip all three and still
  exit 0. The conformance test asserts both fixture coverage and the absence of
  output for those negative cases.

| Fixture | Upstream shape | Transform |
|---|---|---|
| `claude-session.jsonl` | `claude -p --output-format json` | `needle-transform-claude` |
| `codex-session.jsonl` | `codex exec` | `needle-transform-codex` |
| `opencode-session.jsonl` | `opencode run --format json` (plus the adapter's `needle_meta` model line) | `needle-transform-opencode` |
| `omp-session.jsonl` | `omp -p --mode json` | `needle-transform-omp` |

These are synthetic but faithful to the documented per-CLI shapes described
in each transform's module docs. Timestamps are epoch milliseconds where the
upstream uses them (opencode, omp `message_end`) and are absent where the
transform stamps wall-clock time itself (claude, codex, omp tool events).
