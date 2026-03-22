# Agent Event Schema (v1)

This document is the authoritative contract between **output transforms** and
**FABRIC**.  Every line written by an output transform must be a valid JSON
object conforming to this schema.

NEEDLE writes these lines opaquely to disk.  NEEDLE does **not** validate or
parse them at runtime — validation and consumption are FABRIC's concern.

The canonical Rust types live in `src/agent_event.rs`.

---

## Envelope

Every event line shares these top-level fields:

| Field            | Type     | Required | Description                                              |
|------------------|----------|----------|----------------------------------------------------------|
| `schema_version` | integer  | yes      | Always `1` for this version of the schema.               |
| `type`           | string   | yes      | Discriminant — one of the event types listed below.      |
| `ts`             | float    | yes      | Unix epoch timestamp in seconds (fractional for sub-sec).|

All additional fields are event-specific and documented per type below.

---

## Event Types

### `tool_call`

The agent invoked a tool.

| Field  | Type          | Required | Description                                               |
|--------|---------------|----------|-----------------------------------------------------------|
| `tool` | string        | yes      | Tool name (e.g. `"Edit"`, `"Bash"`, `"Read"`).            |
| `path` | string        | no       | File path the tool operates on, if applicable.            |
| `args` | object / null | no       | Tool arguments as a free-form JSON object.                |

**Example:**
```json
{"schema_version":1,"type":"tool_call","tool":"Edit","path":"src/auth.ts","args":{"old_string":"foo","new_string":"bar"},"ts":1711100400.123}
```

---

### `tool_result`

A tool returned its result.

| Field    | Type    | Required | Description                                             |
|----------|---------|----------|---------------------------------------------------------|
| `tool`   | string  | yes      | Name of the tool whose result this is.                  |
| `success`| boolean | yes      | Whether the tool call succeeded.                        |
| `output` | string  | no       | Short human-readable summary of the output.             |

**Example:**
```json
{"schema_version":1,"type":"tool_result","tool":"Edit","success":true,"ts":1711100400.456}
```

---

### `agent_message`

The agent emitted a text turn.

| Field     | Type   | Required | Description                                             |
|-----------|--------|----------|---------------------------------------------------------|
| `role`    | string | yes      | Message author: `"assistant"`, `"user"`, or `"system"`.|
| `content` | string | yes      | Full text content of the message.                       |

**Example:**
```json
{"schema_version":1,"type":"agent_message","role":"assistant","content":"Updated the auth flow","ts":1711100401.0}
```

---

### `tokens`

A token-usage update (cumulative or incremental — transforms choose).

| Field         | Type    | Required | Description                                            |
|---------------|---------|----------|--------------------------------------------------------|
| `input`       | integer | yes      | Number of input (prompt) tokens.                       |
| `output`      | integer | yes      | Number of output (completion) tokens.                  |
| `model`       | string  | yes      | Model identifier (e.g. `"claude-sonnet-4-6"`).         |
| `cache_read`  | integer | no       | Cache read tokens, if reported by the model.           |
| `cache_write` | integer | no       | Cache write tokens, if reported by the model.          |

**Example:**
```json
{"schema_version":1,"type":"tokens","input":1200,"output":450,"model":"claude-sonnet-4-6","ts":1711100403.0}
```

---

### `error`

An agent-level error occurred.

| Field         | Type    | Required | Description                                                          |
|---------------|---------|----------|----------------------------------------------------------------------|
| `message`     | string  | yes      | Human-readable error description.                                    |
| `recoverable` | boolean | yes      | Whether the agent may retry or continue after this error.            |
| `code`        | string  | no       | Optional error category (e.g. `"rate_limit"`, `"context_overflow"`). |

**Example:**
```json
{"schema_version":1,"type":"error","message":"rate limit exceeded","recoverable":true,"ts":1711100410.0}
```

---

## Full Example Stream

```jsonl
{"schema_version":1,"type":"tool_call","tool":"Edit","path":"src/auth.ts","args":{"old_string":"foo","new_string":"bar"},"ts":1711100400.123}
{"schema_version":1,"type":"tool_result","tool":"Edit","success":true,"ts":1711100400.456}
{"schema_version":1,"type":"agent_message","role":"assistant","content":"Updated the auth flow","ts":1711100401.0}
{"schema_version":1,"type":"tokens","input":1200,"output":450,"model":"claude-sonnet-4-6","ts":1711100403.0}
{"schema_version":1,"type":"error","message":"rate limit exceeded","recoverable":true,"ts":1711100410.0}
```

---

## Versioning

- `schema_version` is an integer incremented on every breaking change.
- Additive changes (new optional fields, new event types) do **not** require a
  version bump — consumers must ignore unknown fields.
- The current version is **1**.
