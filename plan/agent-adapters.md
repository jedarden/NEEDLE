# Agent Adapters

NEEDLE is agent-agnostic. It wraps any headless CLI that accepts a prompt and exits. The adapter system is the abstraction layer that makes this possible.

---

## Design Principle

NEEDLE does not know how agents work. It knows how to:
1. Render an invoke template with variables
2. Pipe a prompt via the configured input method
3. Wait for the process to exit
4. Capture exit code, stdout, and stderr

Everything else — authentication, model selection, context handling, tool use — is the agent's responsibility. NEEDLE does not parse, interpret, or modify agent behavior.

---

## Adapter Interface

An adapter is a YAML file that describes how to invoke a specific agent:

```yaml
# ~/.needle/agents/claude-anthropic-sonnet.yaml

name: claude-anthropic-sonnet
description: Claude Code with Anthropic Sonnet model
agent_cli: claude                     # binary name (must be on PATH)
version_command: "claude --version"   # command to check agent version

# How the agent receives its prompt
input_method: stdin                   # stdin | file | args

# The command template executed via bash
invoke_template: >
  cd {workspace} &&
  claude --print
  --model claude-sonnet-4-6
  --max-turns 30
  --output-format json
  < {prompt_file}

# Environment variables set before invocation
environment:
  CLAUDE_CODE_MAX_TURNS: "30"

# Token extraction from agent output (optional, best-effort)
token_extraction:
  method: json_field                  # json_field | regex | none
  input_path: "usage.input_tokens"
  output_path: "usage.output_tokens"

# Provider metadata (for rate limiting and cost tracking)
provider: anthropic
model: claude-sonnet-4-6
```

---

## Input Methods

### stdin

Prompt is piped to the agent's stdin. Most common for Claude Code.

```yaml
input_method: stdin
invoke_template: >
  cd {workspace} && claude --print --model {model} < {prompt_file}
```

NEEDLE writes the prompt to a temp file (`{prompt_file}`) and redirects it to stdin. This avoids shell escaping issues with heredocs.

### file

Prompt is written to a file and the file path is passed as an argument.

```yaml
input_method: file
invoke_template: >
  cd {workspace} && opencode run --prompt-file {prompt_file}
```

### args

Prompt is passed as a command-line argument.

```yaml
input_method: args
invoke_template: >
  cd {workspace} && aider --message {prompt_escaped}
```

`{prompt_escaped}` is the prompt with shell metacharacters escaped. For long prompts, NEEDLE may fall back to file-based input even in args mode.

---

## Template Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `{workspace}` | Absolute path to workspace | `/home/coder/project` |
| `{prompt_file}` | Path to temp file containing the prompt | `/tmp/needle-prompt-a1b2.txt` |
| `{prompt_escaped}` | Shell-escaped prompt string | `Fix the auth bug in src/auth.rs` |
| `{bead_id}` | Current bead ID | `nd-a3f8` |
| `{model}` | Model identifier from adapter config | `claude-sonnet-4-6` |
| `{worker_id}` | Worker identifier | `needle-claude-anthropic-sonnet-alpha` |
| `{timeout}` | Timeout in seconds | `600` |

---

## Built-in Adapters

NEEDLE ships with adapters for common agents, embedded in the binary. These can be overridden by placing a file with the same name in `~/.needle/agents/`.

### Claude Code

```yaml
name: claude-anthropic-sonnet
description: Claude Code (Anthropic Sonnet)
agent_cli: claude
version_command: "claude --version"
input_method: stdin
invoke_template: >
  cd {workspace} &&
  claude --print
  --model claude-sonnet-4-6
  --max-turns 30
  --output-format json
  --verbose
  < {prompt_file}
environment: {}
token_extraction:
  method: json_field
  input_path: "result.usage.input_tokens"
  output_path: "result.usage.output_tokens"
provider: anthropic
model: claude-sonnet-4-6
```

```yaml
name: claude-anthropic-opus
description: Claude Code (Anthropic Opus)
agent_cli: claude
version_command: "claude --version"
input_method: stdin
invoke_template: >
  cd {workspace} &&
  claude --print
  --model claude-opus-4-6
  --max-turns 50
  --output-format json
  --verbose
  < {prompt_file}
environment: {}
token_extraction:
  method: json_field
  input_path: "result.usage.input_tokens"
  output_path: "result.usage.output_tokens"
provider: anthropic
model: claude-opus-4-6
```

### OpenCode

```yaml
name: opencode-default
description: OpenCode with default model
agent_cli: opencode
version_command: "opencode version"
input_method: file
invoke_template: >
  cd {workspace} &&
  opencode run --prompt-file {prompt_file} --non-interactive
environment: {}
token_extraction:
  method: none
provider: configurable
model: configurable
```

### Codex CLI

```yaml
name: codex-openai-gpt4
description: Codex CLI (OpenAI GPT-4)
agent_cli: codex
version_command: "codex --version"
input_method: args
invoke_template: >
  cd {workspace} &&
  codex --model gpt-4 --approval-mode full-auto "{prompt_escaped}"
environment: {}
token_extraction:
  method: none
provider: openai
model: gpt-4
```

### Aider

```yaml
name: aider-anthropic-sonnet
description: Aider with Anthropic Sonnet
agent_cli: aider
version_command: "aider --version"
input_method: args
invoke_template: >
  cd {workspace} &&
  aider --model claude-sonnet-4-6 --yes --message "{prompt_escaped}"
environment: {}
token_extraction:
  method: regex
  pattern: "Tokens: ([\\d,]+) sent, ([\\d,]+) received"
  input_group: 1
  output_group: 2
provider: anthropic
model: claude-sonnet-4-6
```

### Generic (template)

```yaml
name: generic-agent
description: Template for custom agents
agent_cli: my-agent
version_command: "my-agent --version"
input_method: stdin
invoke_template: >
  cd {workspace} && my-agent < {prompt_file}
environment: {}
token_extraction:
  method: none
provider: unknown
model: unknown
```

---

## Prompt Construction

The prompt given to the agent is constructed by the PromptBuilder. It is a deterministic function of the bead state — same bead produces the same prompt.

### Prompt Template

```markdown
## Task

{bead_title}

## Description

{bead_body}

## Workspace

{workspace_path}

## Context Files

{context_file_contents}

## Instructions

{workspace_instructions}

Complete the task described above. When finished:
- Commit your changes with a descriptive message
- Close the bead: `br close {bead_id} --body "Summary of what was done"`

If you cannot complete the task:
- Do NOT close the bead
- The bead will be automatically released for retry

Bead ID: {bead_id}
```

### Context Injection

The prompt includes content from files configured in workspace `.needle.yaml`:

```yaml
prompt:
  context_files:
    - CLAUDE.md
    - AGENTS.md
    - docs/architecture.md
```

These files are read at prompt build time and included verbatim. If a file doesn't exist, it is silently omitted.

### Agent-Owned Closure

The prompt instructs the agent to close the bead via `br close`. NEEDLE does not close beads itself. This is a deliberate design decision based on v1 experience (see `docs/notes/bead-lifecycle-bugs.md` and `docs/notes/operational-fleet-lessons.md`):

- The agent knows whether the work is actually done
- NEEDLE's post-dispatch parsing of agent output was fragile
- Exit code 0 does not guarantee the work was completed correctly
- The agent can include a meaningful closure message

---

## Adapter Validation

```bash
# Test an adapter without processing beads
needle test-agent claude-anthropic-sonnet

# What it does:
# 1. Verifies agent CLI is on PATH
# 2. Runs version command
# 3. Sends a trivial prompt ("echo hello")
# 4. Verifies agent starts and exits cleanly
# 5. Tests token extraction if configured
# 6. Reports results
```

```
$ needle test-agent claude-anthropic-sonnet

  Adapter: claude-anthropic-sonnet
  CLI:     claude (found at /home/coder/.local/bin/claude)
  Version: Claude Code v1.0.30
  Input:   stdin
  Probe:   echo hello → exit 0 (1.2s)
  Tokens:  extraction working (in: 45, out: 12)
  Status:  READY
```

---

## Adding a Custom Agent

To add support for a new agent:

1. Create a YAML file in `~/.needle/agents/`:
   ```bash
   cp ~/.needle/agents/generic-agent.yaml ~/.needle/agents/my-agent.yaml
   ```

2. Edit the file with the agent's invocation details

3. Test the adapter:
   ```bash
   needle test-agent my-agent
   ```

4. Use it:
   ```bash
   needle run --agent my-agent
   ```

No code changes required. No recompilation. No restart of other workers.
