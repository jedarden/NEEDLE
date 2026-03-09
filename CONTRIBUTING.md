# Contributing to NEEDLE

NEEDLE is a universal task queue automation wrapper for headless coding agents. This guide covers everything you need to contribute effectively.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Development Environment Setup](#development-environment-setup)
- [Project Structure](#project-structure)
- [How to Add New Strands](#how-to-add-new-strands)
- [How to Add Agent Adapters](#how-to-add-agent-adapters)
- [Testing Conventions](#testing-conventions)
- [Code Style](#code-style)
- [PR Process](#pr-process)

---

## Architecture Overview

NEEDLE orchestrates CLI-based AI coding agents (Claude Code, OpenCode, Codex, Aider, etc.) by managing task queues, distributing work to parallel workers, and logging effort per task.

### Core Concepts

**Beads** — Tasks stored in a SQLite-backed queue managed by the `br` (beads_rust) CLI. Each bead has a title, description, priority, status, and optional dependencies.

**Strands** — Priority-ordered work-finding strategies. The strand engine tries each strand in sequence and stops when one finds work. This is a waterfall pattern: if a higher-priority strand finds work, lower-priority ones are skipped for that iteration.

**Workers** — Independent processes (typically in tmux sessions) that loop: claim a bead → dispatch to agent → record results.

**Agent Adapters** — YAML configurations that describe how to invoke a specific CLI agent (command, input method, output format, rate limits, cost model).

### Component Flow

```
needle run --agent <name> --workspace <path>
    │
    └─► src/cli/run.sh           (parse args, validate)
            │
            └─► src/runner/loop.sh      (main worker loop)
                    │
                    ├─► src/strands/engine.sh   (try each strand)
                    │       │
                    │       └─► src/strands/<name>.sh
                    │               │
                    │               └─► src/bead/claim.sh  (atomic claim)
                    │                       │
                    │                       └─► src/bead/prompt.sh (build prompt)
                    │
                    └─► src/agent/dispatch.sh   (invoke agent)
                            │
                            ├─► src/agent/loader.sh  (parse YAML config)
                            └─► src/agent/escape.sh  (escape prompt)
```

### Key Subsystems

| Subsystem | Location | Purpose |
|-----------|----------|---------|
| Strand Engine | `src/strands/engine.sh` | Waterfall dispatcher |
| Worker Loop | `src/runner/loop.sh` | Main iteration logic |
| State Registry | `src/runner/state.sh` | Worker registration & counting |
| Rate Limiter | `src/runner/limits.sh` | Per-provider request throttling |
| Config | `src/lib/config.sh` | Load/merge/hot-reload config |
| Telemetry | `src/telemetry/` | Events, tokens, costs, budget |
| Hooks | `src/hooks/runner.sh` | Lifecycle callbacks |
| Bead Mitosis | `src/bead/mitosis.sh` | Split complex beads |
| Lock System | `src/lock/` | File checkout via `/dev/shm` |

---

## Development Environment Setup

### Prerequisites

| Tool | Version | Purpose |
|------|---------|---------|
| `bash` | 4.0+ | Runtime (all code is pure bash) |
| `br` | latest | Bead management CLI (beads_rust) |
| `jq` | 1.6+ | JSON processing |
| `git` | 2.0+ | Version control |
| `tmux` | optional | Multi-worker sessions |
| `fzf` | optional | Interactive bead selection |

Run the bootstrap check to validate your environment:

```bash
bash bootstrap/check.sh
```

### Clone and Configure

```bash
git clone https://github.com/jedarden/NEEDLE.git
cd NEEDLE

# Initialize a workspace (creates .needle/config.yaml)
bin/needle init --workspace .

# Review generated config
cat .needle/config.yaml
```

### Configuration

NEEDLE loads config from (in order, later values override):

1. `~/.needle/config.yaml` (global defaults)
2. `<workspace>/.needle/config.yaml` (workspace config)
3. Environment variables prefixed with `NEEDLE_`

Key config fields:

```yaml
workspace: /path/to/workspace    # Path to the workspace
agent: claude-anthropic-sonnet   # Default agent adapter
strands:
  pluck: true                    # Enable/disable individual strands
  weave: auto                    # "auto" = depends on billing model
  pulse: auto
billing_model: pay_per_token     # pay_per_token | unlimited | use_or_lose
budget:
  daily_limit_usd: 10.00
  warning_threshold: 0.80
```

### Running Locally

```bash
# Single worker, foreground
bin/needle run --agent claude-anthropic-sonnet --workspace /path/to/project

# With debug output
NEEDLE_DEBUG=1 bin/needle run --agent claude-anthropic-sonnet --workspace .

# Dry-run (show what would be claimed, don't execute)
bin/needle run --dry-run --workspace .

# List available agents
bin/needle agents list

# Show bead queue status
bin/needle status --workspace .
```

---

## Project Structure

```
NEEDLE/
├── bin/
│   ├── needle              # Main CLI entry point
│   ├── needle-ready        # Readiness check helper
│   └── needle-db-rebuild   # Database rebuild utility
├── src/
│   ├── agent/              # Agent adapter loading and dispatch
│   │   ├── dispatch.sh     # Render template, invoke agent
│   │   ├── loader.sh       # Parse YAML adapter configs
│   │   └── escape.sh       # Prompt escaping per input method
│   ├── bead/               # Bead lifecycle management
│   │   ├── claim.sh        # Atomic claim with retry
│   │   ├── select.sh       # Weighted priority selection
│   │   ├── prompt.sh       # Build agent prompt from bead
│   │   └── mitosis.sh      # Split complex beads
│   ├── cli/                # One file per CLI subcommand
│   ├── hooks/              # Hook runner and validation
│   ├── lib/                # Shared utilities
│   │   ├── config.sh       # Config loading and merging
│   │   ├── output.sh       # Colored output functions
│   │   ├── constants.sh    # Global constants
│   │   ├── paths.sh        # Path helpers
│   │   ├── billing_models.sh  # Billing model logic
│   │   └── diagnostic.sh   # Starvation diagnostics
│   ├── lock/               # File checkout with /dev/shm locks
│   ├── runner/             # Worker loop and infrastructure
│   │   ├── loop.sh         # Main worker loop
│   │   ├── state.sh        # Worker registration
│   │   ├── tmux.sh         # Tmux session management
│   │   └── limits.sh       # Rate limiting
│   ├── strands/            # Work-finding strategies
│   │   ├── engine.sh       # Strand dispatcher (waterfall)
│   │   ├── pluck.sh        # Claim beads from queue
│   │   ├── explore.sh      # Discover work in workspaces
│   │   ├── mend.sh         # Maintenance & cleanup
│   │   ├── weave.sh        # Create beads from doc gaps
│   │   ├── unravel.sh      # Alternative solutions for blocked beads
│   │   ├── pulse.sh        # Proactive quality monitoring
│   │   └── knot.sh         # Alert humans when stuck
│   ├── telemetry/          # Metrics and cost tracking
│   └── watchdog/           # Heartbeat monitoring
├── config/
│   └── agents/             # Agent adapter YAML files
├── tests/                  # Test suite (60+ files)
├── docs/                   # Design documents and specs
├── hooks/                  # Default hook templates
└── bootstrap/              # Installation scripts
```

---

## How to Add New Strands

Strands are the core work-finding mechanism. Each strand is a bash script exporting one function.

### Strand Interface

A strand must export a function named `_needle_strand_<name>` with this signature:

```bash
# Returns:
#   0 = work was found and executed
#   1 = no work found (fall through to next strand)
_needle_strand_myname() {
  local workspace="$1"
  local agent="$2"
  # ...
}
```

### Step-by-Step: Adding a Strand

**1. Create the strand file**

```bash
# src/strands/myname.sh
#!/usr/bin/env bash

[[ -n "${_NEEDLE_STRAND_MYNAME_LOADED:-}" ]] && return 0
_NEEDLE_STRAND_MYNAME_LOADED=1

# Source dependencies
# shellcheck source=src/lib/output.sh
source "${NEEDLE_ROOT}/src/lib/output.sh"
# shellcheck source=src/bead/claim.sh
source "${NEEDLE_ROOT}/src/bead/claim.sh"

_needle_strand_myname() {
  local workspace="$1"
  local agent="$2"

  _needle_info "myname: searching for work..."

  # Emit telemetry: strand started
  _needle_event "strand.started" "strand=myname"

  # Your work-discovery logic here.
  # Example: look for TODO comments and create beads for them.

  local work_found=0

  # If you create/claim work, return 0
  if [[ $work_found -eq 1 ]]; then
    _needle_event "strand.completed" "strand=myname"
    return 0
  fi

  # No work found — fall through to next strand
  _needle_event "strand.fallthrough" "strand=myname"
  return 1
}
```

**2. Register the strand in the engine**

Open `src/strands/engine.sh` and add your strand to the ordered list:

```bash
# In _needle_strand_engine(), add to the STRANDS array:
local -a STRANDS=(
  pluck
  explore
  mend
  weave
  unravel
  pulse
  myname   # <-- add here in priority order
  knot
)
```

Also source your file near the top of `engine.sh`:

```bash
source "${NEEDLE_ROOT}/src/strands/myname.sh"
```

**3. Add config support**

In `src/lib/config.sh`, add a default for your strand:

```bash
# In the defaults block:
strands__myname="${NEEDLE_STRAND_MYNAME:-auto}"
```

**4. Handle enablement in the engine**

The engine already calls `_needle_strand_is_enabled <name>` before invoking each strand. The `auto` value means it's enabled unless billing constraints prevent it. Add explicit handling only if your strand has unusual enablement logic.

**5. Write tests**

Create `tests/test_myname.sh` following the test conventions below. At minimum, test:
- Returns 1 when no work is found
- Returns 0 when work is found
- Respects the `strands.myname: false` config setting

**6. Document the strand**

Add an entry to this CONTRIBUTING.md strand table and to `docs/plan.md`.

### Strand Design Guidelines

- **Be fast when idle.** If no work exists, the strand should return 1 quickly. Strands run in a tight loop.
- **Emit telemetry.** Use `_needle_event` at start, completion, and fallthrough.
- **Be idempotent.** Multiple workers run simultaneously. Use atomic claim semantics (`br update --claim`) to avoid double-work.
- **Log clearly.** Use `_needle_info`, `_needle_warn`, `_needle_debug` (not `echo`) so output respects color and verbosity settings.
- **Clean up on failure.** If a strand creates intermediate state, clean it up if claiming fails.

---

## How to Add Agent Adapters

Agent adapters are YAML configuration files in `config/agents/`. They describe how to invoke a specific CLI coding agent.

### YAML Schema

```yaml
# config/agents/myagent-provider-model.yaml

name: myagent-provider-model        # Unique identifier (matches filename without .yaml)
runner: myagent                     # CLI executable name (must be in PATH)
provider: myprovider                # Provider name (used for rate limiting grouping)
model: mymodel                      # Model identifier (for logging)

invoke: |
  # Shell template. Available variables:
  #   ${WORKSPACE}   - absolute path to the workspace
  #   ${PROMPT}      - the bead prompt (already escaped for input method)
  #   ${BEAD_ID}     - bead identifier
  #   ${BEAD_TITLE}  - bead title
  #   ${AGENT_DIR}   - directory containing this config file
  cd ${WORKSPACE} && myagent --model ${model} --print <<'NEEDLE_PROMPT'
  ${PROMPT}
  NEEDLE_PROMPT

input:
  method: heredoc     # How the prompt is passed to the agent:
                      #   heredoc - via bash heredoc (default, most compatible)
                      #   stdin   - piped to agent's stdin
                      #   file    - written to temp file, path passed as arg
                      #   args    - appended directly to command line

output:
  format: text        # Agent output format:
                      #   text        - plain text (default)
                      #   json        - single JSON object
                      #   stream-json - newline-delimited JSON (JSONL)
  success_codes: [0]  # Exit codes that mean "task completed"
  retry_codes: [1]    # Exit codes that mean "transient error, retry"
  fail_codes: [2]     # Exit codes that mean "permanent failure"

limits:
  requests_per_minute: 30    # Rate limit for this provider
  max_concurrent: 3          # Max parallel workers using this agent

cost:
  type: pay_per_token        # Cost model: pay_per_token | unlimited | use_or_lose
  input_per_1k: 0.001        # USD per 1k input tokens
  output_per_1k: 0.002       # USD per 1k output tokens
```

### Step-by-Step: Adding an Adapter

**1. Create the YAML file**

```bash
cp config/agents/claude-anthropic-sonnet.yaml config/agents/myagent-provider-model.yaml
```

Edit to match your agent's invocation pattern.

**2. Validate the adapter loads**

```bash
bin/needle agents list           # Should show your adapter
bin/needle agents show myagent-provider-model   # Show parsed config
```

**3. Test manually**

```bash
# Run with a test workspace containing at least one bead
bin/needle run --agent myagent-provider-model --workspace /tmp/test-ws --dry-run
```

**4. Handle stream-json output (optional)**

If your agent emits JSONL events, add token extraction patterns in `src/telemetry/tokens.sh` so NEEDLE can track costs. Look at the existing Claude patterns for reference.

**5. Write tests**

Add test cases to `tests/test_agents.sh` or create a dedicated `tests/test_myagent.sh`. Test that:
- The adapter loads without errors
- The invoke template renders correctly with test variables
- The output format is parsed correctly

### Adapter Design Guidelines

- **Use heredoc input** when your agent supports it — it's the most reliable way to pass multi-line prompts.
- **Set accurate rate limits.** Wrong limits either throttle workers unnecessarily or cause API errors.
- **Set accurate cost values.** They drive budget tracking and billing model decisions.
- **Classify exit codes carefully.** A wrong `retry_codes` entry can cause infinite retry loops.
- **Test with `--dry-run`** before running against a real workspace.

---

## Testing Conventions

### Test Structure

All tests live in `tests/`. Each file tests one module. The pattern is:

```bash
#!/usr/bin/env bash
set -o pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$TEST_DIR")"

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

test_start() {
  ((TESTS_RUN++))
  echo -n "Testing: $1... "
}

test_pass() {
  echo "PASS"
  ((TESTS_PASSED++))
}

test_fail() {
  local msg="${1:-}"
  echo "FAIL${msg:+ ($msg)}"
  ((TESTS_FAILED++))
}

# --- Setup ---
setup() {
  WORK_DIR="$(mktemp -d)"
  # Initialize test fixtures
}

teardown() {
  rm -rf "$WORK_DIR"
}

# --- Tests ---
test_my_feature() {
  test_start "my_feature does X when Y"

  # Arrange
  local input="..."

  # Act
  local result
  result=$(my_function "$input")

  # Assert
  if [[ "$result" == "expected" ]]; then
    test_pass
  else
    test_fail "got: $result"
  fi
}

# --- Run ---
setup
test_my_feature
teardown

echo ""
echo "Results: $TESTS_PASSED/$TESTS_RUN passed"
[[ $TESTS_FAILED -eq 0 ]] && exit 0 || exit 1
```

### Mocking External Dependencies

Tests must not call real external tools (`br`, agents, APIs). Mock them:

```bash
# Mock br CLI
br() {
  case "$*" in
    "list --status open"*)
      echo '{"id":"nd-test","title":"Test bead","priority":2}'
      return 0
      ;;
    "update --claim"*)
      return 0
      ;;
    *)
      echo "MOCK br: unhandled call: $*" >&2
      return 1
      ;;
  esac
}
export -f br

# Mock agent functions
_needle_dispatch_agent() {
  echo "0|1234|/tmp/agent-output"
}
export -f _needle_dispatch_agent
```

### Running Tests

```bash
# Run all tests
bash tests/run_all.sh

# Run a single test file
bash tests/test_pluck.sh

# Run tests matching a pattern
for f in tests/test_strand_*.sh; do bash "$f"; done
```

### Test Coverage Expectations

- Every strand must have a test file: `tests/test_<strandname>.sh`
- Every agent adapter feature must be covered in `tests/test_dispatch.sh`
- New config options must be covered in `tests/test_config.sh`
- New CLI subcommands must have a test file: `tests/test_<command>.sh`

### Writing Good Tests

- **One assertion per test function.** Keep tests focused.
- **Use temp directories.** Never write to the real workspace or `~/.needle` in tests.
- **Clean up.** Use `trap teardown EXIT` to ensure cleanup even on failure.
- **Test edge cases.** Empty queues, missing config, concurrent access, timeouts.
- **Name tests descriptively.** `test_pluck_returns_0_when_bead_claimed` not `test_pluck_1`.

---

## Code Style

NEEDLE is written entirely in Bash. Follow these conventions:

### Shebang and Error Handling

```bash
#!/usr/bin/env bash
# Main executables: use strict mode
set -euo pipefail

# Sourced modules: use pipefail only (callers control -e and -u)
set -o pipefail
```

### Module Guard

Every sourced file must have a load guard to prevent double-sourcing:

```bash
[[ -n "${_NEEDLE_MYMODULE_LOADED:-}" ]] && return 0
_NEEDLE_MYMODULE_LOADED=1
```

### Naming Conventions

| Type | Convention | Example |
|------|-----------|---------|
| Public functions | `_needle_<module>_<action>` | `_needle_strand_pluck` |
| Private functions | `_<module>_<action>` | `_pluck_find_candidates` |
| Global constants | `NEEDLE_<NAME>` (UPPER_CASE) | `NEEDLE_ROOT` |
| Local variables | `lower_case` | `local bead_id` |
| Config keys | `strands__<name>` (double underscore for nesting) | `strands__pluck` |

### Variable Declarations

```bash
# Always declare locals in functions
_needle_mymodule_do_thing() {
  local workspace="$1"
  local agent="${2:-}"    # Optional with default
  local result

  result=$(some_command "$workspace")
  echo "$result"
}

# Prefer local -r for constants in functions
local -r config_file="${workspace}/.needle/config.yaml"

# Use -a for arrays, -A for associative arrays
local -a items=()
local -A map=()
```

### Output Functions

Never use raw `echo` for user-facing messages. Use the output library:

```bash
source "${NEEDLE_ROOT}/src/lib/output.sh"

_needle_info "Normal status message"       # → stdout, always shown
_needle_success "Operation completed"      # → stdout, green
_needle_warn "Something looks off"         # → stderr, yellow
_needle_error "Something failed"           # → stderr, red
_needle_debug "Internal state: $var"       # → stderr, only with NEEDLE_DEBUG=1
_needle_verbose "Extra detail"             # → stderr, only with NEEDLE_VERBOSE=1
```

### Error Handling

```bash
# Prefer early return over nested ifs
_needle_do_thing() {
  local workspace="$1"

  if [[ ! -d "$workspace" ]]; then
    _needle_error "Workspace not found: $workspace"
    return 1
  fi

  # ... rest of function
}

# Use || for simple error cases
br update "$bead_id" --status failed || {
  _needle_warn "Failed to update bead status"
}
```

### Quoting

```bash
# Always double-quote variable expansions
local path="${NEEDLE_ROOT}/src"

# Quote command substitutions
local result
result="$(some_command "$arg")"

# Exception: arithmetic and [[ comparisons with = or == are safe unquoted
if [[ $count -eq 0 ]]; then ...
if [[ $name == "pluck" ]]; then ...
```

### Sourcing Dependencies

```bash
# Use shellcheck directives so IDE tooling works
# shellcheck source=src/lib/output.sh
source "${NEEDLE_ROOT}/src/lib/output.sh"
```

### ShellCheck

Run ShellCheck on any file you modify:

```bash
shellcheck src/strands/myname.sh
shellcheck tests/test_myname.sh
```

Fix all warnings before opening a PR. Disable specific checks only with a comment explaining why:

```bash
# shellcheck disable=SC2086  # Word splitting intentional: $flags may have multiple words
eval "$runner" $flags "$workspace"
```

---

## PR Process

### Before Opening a PR

1. **Run all tests locally:**
   ```bash
   bash tests/run_all.sh
   ```

2. **Run ShellCheck on changed files:**
   ```bash
   shellcheck $(git diff --name-only HEAD | grep '\.sh$')
   ```

3. **Test your change manually** against a real workspace if possible.

4. **Update documentation** if you added a strand, adapter, config option, or CLI command.

### Commit Format

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<bead-id>): <short description>

[optional body]

Co-Authored-By: Your Name <you@example.com>
```

Types: `feat`, `fix`, `chore`, `docs`, `test`, `refactor`, `perf`

Examples:
```
feat(nd-abc1): Add pulse strand for security scanning
fix(nd-xyz2): Handle empty queue in pluck strand
docs(nd-p7wn): Create CONTRIBUTING.md developer guide
test(nd-def3): Add coverage for agent dispatch retry logic
```

### PR Title and Description

- **Title:** Same format as commit (`type(bead-id): description`)
- **Description:** Include:
  - What changed and why
  - How to test
  - Link to the bead: `Closes nd-<id>`
  - Screenshots or log excerpts if relevant

### Review Criteria

PRs are reviewed for:

- [ ] Tests pass (CI runs `bash tests/run_all.sh`)
- [ ] ShellCheck clean
- [ ] Module guard present in new sourced files
- [ ] Naming conventions followed
- [ ] Output uses `_needle_*` functions (not `echo`)
- [ ] External dependencies mocked in tests
- [ ] Temp files cleaned up
- [ ] Telemetry events emitted for new strand operations
- [ ] Config option documented in CONTRIBUTING.md and/or help text

### Bead Workflow

NEEDLE uses its own bead system to track development work:

```bash
# Create a bead for your feature
br create --title "Add fuzzy search to explore strand" --description "..."

# Work on it
br update nd-xxxx --status in_progress

# Close when done (usually done by the PR merge hook)
br close nd-xxxx --status completed
```

Reference the bead ID in your branch name and commits: `feat/nd-xxxx-fuzzy-explore`.

---

## Common Patterns

### Reading Config Values

```bash
source "${NEEDLE_ROOT}/src/lib/config.sh"

# Load config for a workspace
_needle_config_load "$workspace"

# Access a value
local agent
agent="$(_needle_config_get "agent")"

# Check strand enablement
if _needle_strand_is_enabled "myname"; then
  # ...
fi
```

### Claiming a Bead

```bash
source "${NEEDLE_ROOT}/src/bead/claim.sh"

local claim_result
if claim_result="$(_needle_bead_claim "$workspace")"; then
  local bead_id status
  bead_id="$(echo "$claim_result" | jq -r '.id')"
  # dispatch agent with this bead
else
  # no beads available
  return 1
fi
```

### Emitting Telemetry Events

```bash
source "${NEEDLE_ROOT}/src/telemetry/events.sh"

_needle_event "my.event" "key1=value1" "key2=value2"
# Writes JSON line to .needle/logs/events.jsonl
```

### Creating Beads Programmatically

```bash
# Create a follow-up bead from within a strand
local new_id
new_id="$(br create \
  --title "Fix identified issue in $file" \
  --description "Found while running pulse scan: $detail" \
  --priority 2 \
  | grep -oP 'Created issue \K[a-z0-9-]+')"

_needle_info "Created follow-up bead: $new_id"
```

---

## Getting Help

- **Docs:** `docs/plan.md` — comprehensive design specification
- **Tests:** `tests/README.md` — detailed test guide
- **Issues:** Open a bead: `br create --title "Question about X"`
- **Architecture:** `docs/` directory contains design documents for major subsystems
