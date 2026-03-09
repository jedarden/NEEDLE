# Agent Configuration

NEEDLE agent configurations are YAML files that define how to invoke a specific AI agent CLI. Each agent has its own config directory containing a YAML file and an optional `stream-parser.sh` script.

## Directory Layout

Agent configs live in the NEEDLE config directory:

```
config/agents/                        # Built-in agents (shipped with NEEDLE)
├── claude-anthropic-sonnet.yaml
├── claude-anthropic-opus.yaml
├── claude-code-glm-4.7.yaml
├── stream-parser.sh                  # Shared Claude Code stream-parser
└── ...

~/.needle/agents/                     # User-defined agents (optional)
├── claude-sonnet/
│   ├── agent.yaml
│   └── stream-parser.sh             # Agent-specific output parser
├── claude-opus/
│   ├── agent.yaml
│   └── stream-parser.sh             # Can be symlinked if same format
└── opencode-deepseek/
    ├── agent.yaml
    └── stream-parser.sh             # Different parser for OpenCode output
```

## YAML Schema

### Minimal Example

```yaml
name: my-agent
description: Brief description of this agent
version: "1.0"
runner: claude
provider: anthropic
model: sonnet

invoke: |
  cd ${WORKSPACE} && \
  claude --print \
         --dangerously-skip-permissions \
  <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT

input:
  method: heredoc

output:
  format: text
  success_codes: [0]
```

### Full Schema Reference

```yaml
# ── Metadata ──────────────────────────────────────────────────────────────────

# Internal identifier (must be unique, no spaces)
name: claude-anthropic-sonnet

# Human-readable description
description: Claude Code with Anthropic Sonnet model

# Config schema version
version: "1.0"

# ── CLI Identification ─────────────────────────────────────────────────────────

# Executable name on PATH
runner: claude

# Provider name (used for rate limiting and cost tracking)
# Values: anthropic | openai | ollama | glm | custom
provider: anthropic

# Short model identifier (used for display and detection)
model: sonnet

# ── Invoke Template ────────────────────────────────────────────────────────────

# Shell command template to run the agent.
# Available variables:
#   ${WORKSPACE}  - absolute path to working directory
#   ${PROMPT}     - task prompt text
#   ${BEAD_ID}    - bead identifier (e.g., nd-abc1)
#   ${BEAD_TITLE} - bead title string
#   ${AGENT_DIR}  - directory containing this agent's YAML + stream-parser.sh
invoke: |
  cd ${WORKSPACE} && \
  unset CLAUDECODE && \
  ANTHROPIC_MODEL=claude-sonnet-4-5-20250929 \
  claude --print \
         --dangerously-skip-permissions \
         --output-format stream-json \
         --verbose \
  <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT
  | ${AGENT_DIR}/stream-parser.sh

# ── Input Method ───────────────────────────────────────────────────────────────

input:
  # How the prompt is passed to the agent CLI
  # Options:
  #   heredoc  - NEEDLE_PROMPT heredoc block (default for Claude Code)
  #   stdin    - piped to stdin
  #   file     - written to a temp file, path passed via env/arg
  #   args     - passed as a CLI argument (requires arg_flag)
  method: heredoc

  # For file method only: path template for the prompt file
  # file_path: /tmp/needle-prompt-${BEAD_ID}.txt

  # For args method only: the flag to prefix the prompt with
  # arg_flag: --message

# ── Output Parsing ─────────────────────────────────────────────────────────────

output:
  # Output format emitted by the agent CLI (before stream-parser)
  # Options:
  #   stream-json  - Claude Code JSONL streaming (recommended)
  #   json         - single JSON object
  #   text         - plain text
  format: stream-json

  # Enable terminal output via tee pipeline
  streaming_enabled: true

  # Exit codes for dispatch decisions
  success_codes: [0]    # Normal completion
  retry_codes:   [1]    # Transient errors (rate limit, network)
  fail_codes:  [2, 137] # Fatal errors (OOM, bad config)

# ── Rate Limits ────────────────────────────────────────────────────────────────

limits:
  requests_per_minute: 60
  max_concurrent: 5

# ── Cost Configuration ─────────────────────────────────────────────────────────

cost:
  # type options:
  #   pay_per_token  - billed per token (most cloud APIs)
  #   unlimited      - free tier or subscription with no per-token cost
  #   use_or_lose    - prepaid credits that expire
  type: pay_per_token
  input_per_1k:  0.003   # USD per 1K input tokens
  output_per_1k: 0.015   # USD per 1K output tokens
```

## Invoke Template Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `${WORKSPACE}` | Absolute path to working directory | `/home/coder/myrepo` |
| `${PROMPT}` | Full task prompt text | `Fix the login bug in...` |
| `${BEAD_ID}` | Bead identifier | `nd-abc1` |
| `${BEAD_TITLE}` | Bead title | `Fix login bug` |
| `${AGENT_DIR}` | Agent config directory (contains `stream-parser.sh`) | `/home/coder/NEEDLE/config/agents` |

## Stream-Parser Piping

### Why a Stream-Parser?

Agents like Claude Code emit raw JSONL when using `--output-format stream-json`. This is machine-readable but hard to read in a terminal. The stream-parser converts JSONL events into human-readable, colorized terminal output.

```
Agent CLI  ──(stream-json JSONL)──>  stream-parser.sh  ──>  human-readable terminal
                                                        ──>  optional heartbeat via NEEDLE_HEARTBEAT_CMD
```

NEEDLE captures the **raw JSONL** (before the stream-parser) via `tee` for token/cost extraction. The stream-parser output is only for terminal display — NEEDLE remains agent-agnostic.

### Invoke Template Pattern

The recommended pattern for Claude Code agents:

```yaml
invoke: |
  cd ${WORKSPACE} && \
  unset CLAUDECODE && \
  ANTHROPIC_MODEL=claude-sonnet-4-5-20250929 \
  claude --print \
         --dangerously-skip-permissions \
         --output-format stream-json \
         --verbose \
  <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT
  | ${AGENT_DIR}/stream-parser.sh
```

Key points:
- `unset CLAUDECODE` prevents nested session detection when running inside Claude Code
- `--output-format stream-json` enables JSONL streaming
- `<<'NEEDLE_PROMPT' ... NEEDLE_PROMPT` passes the prompt via heredoc (quoted delimiter prevents variable expansion)
- `| ${AGENT_DIR}/stream-parser.sh` pipes raw JSONL through the agent's formatter
- `${AGENT_DIR}` resolves to the directory containing the agent YAML, making it self-contained

### Stream-Parser Location

Each agent's `stream-parser.sh` lives alongside its YAML config:

```
config/agents/
├── claude-anthropic-sonnet.yaml
└── stream-parser.sh              ← ${AGENT_DIR}/stream-parser.sh resolves here
```

For user-defined agents:

```
~/.needle/agents/my-agent/
├── agent.yaml
└── stream-parser.sh              ← ${AGENT_DIR}/stream-parser.sh resolves here
```

The script **must be executable** (`chmod +x stream-parser.sh`).

### Stream-Parser Environment

| Variable | Description |
|----------|-------------|
| `NEEDLE_HEARTBEAT_CMD` | Shell command to run periodically (optional). Used to keep the watchdog alive during long tasks. |
| `NO_COLOR` | Disable color output when set (any value). |
| `TERM` | Terminal type. Set to `dumb` to disable colors. |

### Heartbeat Integration

If `NEEDLE_HEARTBEAT_CMD` is set, the stream-parser should emit a heartbeat every ~30 seconds to prevent watchdog timeouts during long-running tasks:

```bash
# In stream-parser.sh
_maybe_heartbeat() {
    if [[ -n "${NEEDLE_HEARTBEAT_CMD:-}" ]]; then
        local now
        now=$(date +%s)
        if (( now - _last_heartbeat_time >= 30 )); then
            eval "$NEEDLE_HEARTBEAT_CMD" 2>/dev/null || true
            _last_heartbeat_time="$now"
        fi
    fi
}

# Call after processing each JSONL line
while IFS= read -r line; do
    # ... process line ...
    _maybe_heartbeat
done
```

### Minimal Stream-Parser Example

```bash
#!/usr/bin/env bash
# stream-parser.sh - Minimal Claude Code stream-json parser

_last_heartbeat_time=0

_maybe_heartbeat() {
    [[ -z "${NEEDLE_HEARTBEAT_CMD:-}" ]] && return
    local now; now=$(date +%s)
    if (( now - _last_heartbeat_time >= 30 )); then
        eval "$NEEDLE_HEARTBEAT_CMD" 2>/dev/null || true
        _last_heartbeat_time="$now"
    fi
}

while IFS= read -r line; do
    [[ -z "$line" ]] && continue

    TYPE=$(printf '%s' "$line" | jq -r '.type // empty' 2>/dev/null)
    [[ -z "$TYPE" ]] && continue

    case "$TYPE" in
        system)
            echo "[system] Session initialized"
            ;;
        assistant)
            CONTENT=$(printf '%s' "$line" | jq -c '.message.content // []' 2>/dev/null)
            BLOCK_TYPE=$(printf '%s' "$CONTENT" | jq -r '.[0].type // empty' 2>/dev/null)
            case "$BLOCK_TYPE" in
                tool_use)
                    TOOL=$(printf '%s' "$CONTENT" | jq -r '.[0].name // empty' 2>/dev/null)
                    echo "  ▶ Tool: $TOOL"
                    ;;
                text)
                    TEXT=$(printf '%s' "$CONTENT" | jq -r '.[0].text // empty' 2>/dev/null)
                    [[ -n "$TEXT" ]] && echo "$TEXT"
                    ;;
            esac
            ;;
        result)
            COST=$(printf '%s' "$line" | jq -r '.cost_usd // "?"' 2>/dev/null)
            echo "══ Result (cost: \$$COST) ══"
            ;;
    esac

    _maybe_heartbeat
done
```

For a full-featured reference implementation, see:
- `config/agents/stream-parser.sh` (ships with NEEDLE)
- `~/ardenone-cluster/agents/claude-code-sonnet/stream-parser.sh` (Marathon Coding reference)

## Input Methods

### heredoc (recommended for Claude Code)

Passes the prompt via a shell heredoc. The quoted delimiter (`'NEEDLE_PROMPT'`) prevents variable expansion inside the heredoc, so the raw prompt text is passed literally:

```yaml
invoke: |
  claude --print <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT
input:
  method: heredoc
```

### args

Passes the prompt as a CLI argument. The prompt is automatically escaped for safe embedding in a double-quoted string:

```yaml
invoke: |
  aider --yes-always --message "${PROMPT}"
input:
  method: args
  arg_flag: --message
```

### file

Writes the prompt to a temp file and passes the path:

```yaml
invoke: |
  opencode --headless --prompt-file ${PROMPT_FILE}
input:
  method: file
  file_path: /tmp/needle-prompt-${BEAD_ID}.txt
```

### stdin

Pipes the prompt to the agent's stdin:

```yaml
invoke: |
  my-agent --interactive
input:
  method: stdin
```

## Agent Examples

### Claude Code (Anthropic)

```yaml
name: claude-anthropic-sonnet
description: Claude Code with Anthropic Sonnet model
version: "1.0"
runner: claude
provider: anthropic
model: sonnet

invoke: |
  cd ${WORKSPACE} && \
  unset CLAUDECODE && \
  ANTHROPIC_MODEL=claude-sonnet-4-5-20250929 \
  claude --print \
         --dangerously-skip-permissions \
         --output-format stream-json \
         --verbose \
  <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT
  | ${AGENT_DIR}/stream-parser.sh

input:
  method: heredoc

output:
  format: stream-json
  streaming_enabled: true
  success_codes: [0]
  retry_codes: [1]
  fail_codes: [2, 137]

limits:
  requests_per_minute: 60
  max_concurrent: 5

cost:
  type: pay_per_token
  input_per_1k: 0.003
  output_per_1k: 0.015
```

### Claude Code (Custom API Base)

For agents using an API proxy or alternative endpoint:

```yaml
name: claude-code-glm
description: Claude Code CLI with GLM model via proxy
version: "1.0"
runner: claude
provider: glm
model: glm-4

invoke: |
  cd ${WORKSPACE} && \
  unset CLAUDECODE && \
  ANTHROPIC_BASE_URL=http://my-proxy:8080/v1 \
  ANTHROPIC_MODEL=glm-4-flash \
  claude --print \
         --dangerously-skip-permissions \
         --output-format stream-json \
         --verbose \
  <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT
  | ${AGENT_DIR}/stream-parser.sh

input:
  method: heredoc

output:
  format: stream-json
  streaming_enabled: true
  success_codes: [0]
  retry_codes: [1]
  fail_codes: [2, 137]

cost:
  type: unlimited
```

### Aider (text output)

```yaml
name: aider-ollama-deepseek
description: Aider with local Deepseek model via Ollama
version: "1.0"
runner: aider
provider: ollama
model: deepseek-coder

invoke: |
  cd ${WORKSPACE} && \
  AIDER_MODEL=ollama:deepseek-coder:latest \
  aider --yes-always \
        --no-pretty \
        --message "${PROMPT}"

input:
  method: args
  arg_flag: --message

output:
  format: text
  success_codes: [0]
  retry_codes: [1]

cost:
  type: unlimited
```

## Related Documentation

- [Streaming Architecture](streaming.md) - How stream-json output and stream-parser piping work
- [FABRIC Integration](fabric-integration.md) - Real-time event forwarding to dashboards
- [Telemetry Events](telemetry-events.md) - Event types and token extraction
