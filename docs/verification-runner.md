# Verification Runner

A configurable, YAML-based verification runner that executes definition-of-done
checks with proper failure aggregation.

## Overview

The verification runner (`scripts/verification-runner.sh`) is a reusable
alternative to embedding checks in shell code. Declare each check in YAML; the
runner loads it, invokes each command with its arguments intact, and reports
every result.

**When to use:**
- You want a reusable verification pattern across multiple repositories
- You prefer declarative configuration over shell script edits
- You need different verification profiles that can be swapped without code changes
- You're building tooling that generates or manipulates verification configurations

**When to stick with `definition-of-done.sh`:**
- You have a single repository with stable verification requirements
- You prefer the simplicity of a single bash script
- Your checks are simple shell commands that don't benefit from YAML structure

## Lane Selection

The verification runner supports **fast lane**, **slow lane**, and **all lanes** modes to support different verification scenarios:

### Lane Definitions

**Fast Lane** - Quick checks that complete in under 2 minutes:
- **Purpose**: Pre-commit validation, quick feedback loops
- **Typical checks**: Code formatting, linting, compilation verification
- **When to use**: Local development, pre-commit hooks, quick PR validation

**Slow Lane** - Comprehensive test suite that may take several minutes:
- **Purpose**: Full validation of codebase correctness
- **Typical checks**: Unit tests, integration tests, documentation tests
- **When to use**: CI pipelines, pre-merge validation, release gates

**All Lanes** - Complete verification (fast then slow):
- **Purpose**: End-to-end validation
- **Execution**: Runs fast lane first, then slow lane sequentially
- **When to use**: Full CI runs, release verification, comprehensive validation

### NEEDLE Lane Configuration

The NEEDLE project organizes checks as follows:

**Fast Lane (3 checks, ~2 minutes):**
1. **Format check** (`cargo fmt --check`) - Verifies code formatting with rustfmt (30s timeout)
2. **Clippy linting** (`cargo clippy --all-targets -- -D warnings`) - Runs clippy lints with deny warnings (60s timeout)
3. **Cargo check** (`cargo check`) - Verifies compilation without running tests (60s timeout)

**Slow Lane:**
The committed NEEDLE configuration names the test-target build, unit tests,
five integration suites, and installer tests. See `.verification/config.yaml`
for the authoritative list and timeouts.

### Usage Examples

```bash
# Quick pre-commit check (fast lane only)
./scripts/verification-runner.sh --fast

# Full test suite run (slow lane only)
./scripts/verification-runner.sh --slow

# Complete verification (both lanes, default)
./scripts/verification-runner.sh --all
./scripts/verification-runner.sh  # Same as --all
```

### Default Behavior

The default mode is `--all` (both lanes). This ensures comprehensive verification by default. For quicker feedback during development, explicitly use `--fast`.

## Quick Start

1. Create a configuration file:
   ```bash
   # Either .verification/config.yaml (preferred) or definition-of-done.yaml
   mkdir -p .verification
   cat > .verification/config.yaml <<'EOF'
   version: "1.0"

   fast_lane:
     - name: "Format check"
       command: "cargo"
       args: ["fmt", "--check"]
       timeout: 30

   slow_lane:
     - name: "Unit tests"
       command: "cargo"
       args: ["test", "--lib"]
       timeout: 900
   EOF
   ```

2. Run verification:
   ```bash
   # Run all lanes (default)
   ./scripts/verification-runner.sh

   # Run fast lane only
   ./scripts/verification-runner.sh --fast

   # Run slow lane only
   ./scripts/verification-runner.sh --slow
   ```

## Configuration Format

### Top-Level Structure

```yaml
version: "1.0"              # Required: Configuration format version
description: |               # Optional: Human-readable description
  This configuration defines the verification checks for this repository.

fast_lane:                  # Optional: Fast checks (seconds, local cgroup)
  - name: "Check name"
    command: "command"
    args: ["arg1", "arg2"]
    timeout: 30

slow_lane:                  # Optional: Slow checks (tests, integration)
  - name: "Test name"
    command: "command"
    args: ["arg1", "arg2"]
    timeout: 900
```

### Check Configuration

Each check in `fast_lane` or `slow_lane` supports:

| Field | Required | Type | Description |
|-------|----------|------|-------------|
| `name` | Yes | string | Human-readable name for the check |
| `command` | Yes | string | Command to execute (must be in PATH) |
| `args` | No | array | Arguments passed to the command (default: `[]`) |
| `timeout` | No | number | Maximum seconds before check is aborted (default: 60) |
| `description` | No | string | Optional description of what the check verifies |
| `allow_failure` | No | boolean | If true, failure doesn't cause overall failure (default: false) |
| `environment` | No | array | `KEY=value` entries set only while the check runs |

### Example Configurations

#### Rust Project (NEEDLE-style)

```yaml
version: "1.0"

fast_lane:
  - name: "Format check"
    command: "cargo"
    args: ["fmt", "--check"]
    timeout: 30
    description: "Verify code formatting"

  - name: "Clippy linting"
    command: "cargo"
    args: ["clippy", "--all-targets", "--", "-D", "warnings"]
    timeout: 60

  - name: "Cargo check"
    command: "cargo"
    args: ["check"]
    timeout: 120

slow_lane:
  - name: "Unit tests"
    command: "cargo"
    args: ["test", "--lib"]
    timeout: 900

  - name: "Integration tests"
    command: "cargo"
    args: ["test", "--test", "integration_tests"]
    timeout: 900
```

#### Go Project

```yaml
version: "1.0"

fast_lane:
  - name: "Go fmt check"
    command: "gofmt"
    args: ["-l", "."]
    timeout: 10
    allow_failure: true  # Warn only, don't fail

  - name: "Go vet"
    command: "go"
    args: ["vet", "./..."]
    timeout: 30

  - name: "Go tests (short)"
    command: "go"
    args: ["test", "./...", "-short"]
    timeout: 60

slow_lane:
  - name: "Go tests (full)"
    command: "go"
    args: ["test", "./..."]
    timeout: 300
```

#### TypeScript/Node Project

```yaml
version: "1.0"

fast_lane:
  - name: "TypeScript check"
    command: "npm"
    args: ["run", "typecheck"]
    timeout: 30

  - name: "Linting"
    command: "npm"
    args: ["run", "lint"]
    timeout: 30

  - name: "Format check"
    command: "npm"
    args: ["run", "format:check"]
    timeout: 10

slow_lane:
  - name: "Unit tests"
    command: "npm"
    args: ["test", "--", "--coverage"]
    timeout: 120

  - name: "Build"
    command: "npm"
    args: ["run", "build"]
    timeout: 180
```

## Command-Line Options

| Option | Description |
|--------|-------------|
| `--fast` | Run fast lane only |
| `--slow` | Run slow lane only |
| `--all` | Run both fast and slow lanes (default) |
| `--config PATH` | Load configuration from specific path |
| `--json PATH` | Write a machine-readable JSON report |
| `--verbose` | Show detailed output from each check |
| `--dry-run` | Validate and list checks without executing |
| `--count-bypass` | Record pre-commit state when the NEEDLE helper is present |
| `--no-verify` | Explicitly skip checks and record a bypass |
| `--help` | Show help message |

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | All checks passed |
| 1 | One or more checks failed |
| 2 | Configuration, dependency, or report-output error |
| 3 | Invalid arguments |

## Behavior

### Failure Aggregation

The runner executes **all checks** even if some fail, collecting failures into a final report:

```bash
$ ./scripts/verification-runner.sh --fast
Running: Format check...
✓ Format check passed
Running: Clippy linting...
✗ Clippy linting failed (exit code: 1)
Running: Cargo check...
✓ Cargo check passed

=== Verification Summary ===
Lane: fast
Checks run: 3
Passed: 2
Failed: 1

Failed checks:
  [2] Clippy linting
      Status: failed
      Exit code: 1

❌ Verification failed
```

### Timeout Handling

Each check has a timeout specified in seconds. If a check exceeds its timeout, it is terminated and marked as failed:

```bash
Running: Unit tests...
✗ Unit tests failed (exit code: 124)  # 124 = timeout
```

### Dry Run Mode

Use `--dry-run` to validate the YAML and see what would execute without running
checks. Dry-run results are reported as `skipped` in JSON:

```bash
$ ./scripts/verification-runner.sh --dry-run --fast
[dry run] would run with 30s timeout
[dry run] would run with 60s timeout
[dry run] would run with 120s timeout
```

## Configuration Discovery

The runner searches for configuration files in this order:

1. `.verification/config.yaml` (preferred)
2. `definition-of-done.yaml`
3. `.verification/config.yml`
4. `definition-of-done.yml`

An explicit `--config PATH` overrides discovery. Relative paths are resolved
from the repository root.

This allows you to:
- Keep verification configs in a dedicated `.verification/` directory
- Provide a repo-wide default at `definition-of-done.yaml`
- Override with custom configs via command line

## Integration Points

### Pre-commit Hook

```bash
#!/usr/bin/env bash
# .githooks/pre-commit

./scripts/verification-runner.sh --fast --count-bypass || {
  echo "Pre-commit verification failed. Use --no-verify to bypass."
  exit 1
}
```

### CI Pipeline

```bash
#!/usr/bin/env bash
# CI step

# Run both lanes (full verification)
./scripts/verification-runner.sh --all --verbose
```

### NEEDLE Gate

```yaml
# .needle.yaml
gates:
  - type: command
    commands:
      - scripts/verification-runner.sh --fast
```

## Requirements

- Bash 4.0+
- `yq` for YAML parsing ([https://github.com/mikefarah/yq](https://github.com/mikefarah/yq))
- `jq` for validation and JSON reports
- GNU `timeout` for per-check limits
- Git repository (for repo root detection)

## Migration from definition-of-done.sh

If you're currently using `scripts/definition-of-done.sh`, here's how to migrate:

1. Export existing checks to YAML:
   ```bash
   # Create new config
   cat > .verification/config.yaml <<'EOF'
   version: "1.0"
   
   fast_lane:
     - name: "Format check"
       command: "cargo"
       args: ["fmt", "--check"]
       timeout: 30
   
     # ... other fast lane checks
   
   slow_lane:
     - name: "Unit tests"
       command: "cargo"
       args: ["test", "--lib"]
       timeout: 900
   
     # ... other slow lane checks
   EOF
   ```

2. Test the new runner:
   ```bash
   ./scripts/verification-runner.sh --fast --dry-run
   ./scripts/verification-runner.sh --fast
   ```

3. Update integration points:
   - Pre-commit hook: change `definition-of-done.sh` to `verification-runner.sh`
   - CI: update workflow to call `verification-runner.sh --all`
   - NEEDLE gate: update `.needle.yaml` to use new script

4. Remove the old script (optional):
   ```bash
   rm scripts/definition-of-done.sh
   ```

## Advanced Usage

### Environment Variables

Pass environment variables to specific checks:

```yaml
fast_lane:
  - name: "Test with custom RUSTFLAGS"
    command: "cargo"
    args: ["test", "--lib"]
    timeout: 120
    environment: ["RUSTFLAGS=-D warnings", "CARGO_TERM_COLOR=always"]
```

### Conditional Checks

Use `allow_failure` for checks that should warn but not fail:

```yaml
fast_lane:
  - name: "Security audit"
    command: "cargo"
    args: ["audit"]
    timeout: 30
    allow_failure: true  # Warn only, don't gate on this
```

## Troubleshooting

**Problem:** `yq`, `jq`, or `timeout` is not found

**Solution:** Install the missing host dependency. The runner uses mikefarah
`yq`, `jq`, and GNU `timeout`.
```bash
# Linux
wget https://github.com/mikefarah/yq/releases/latest/download/yq_linux_amd64 -O /usr/local/bin/yq
chmod +x /usr/local/bin/yq

# macOS
brew install yq
```

**Problem:** Configuration file not found

**Solution:** The runner searches for `.verification/config.yaml` or `definition-of-done.yaml`. Create one or specify `--config PATH`.

**Problem:** Check exits with code 124

**Solution:** This means the check timed out. Increase the timeout in your configuration or optimize the check.

## Bypass detection

`SKIP_CHECKS=1`, `VERIFICATION_SKIP=1`, `NEEDLE_SKIP_VERIFICATION=1`, and the
explicit `--no-verify` option skip execution, print a warning, and append a
JSON Lines event to `.beads/bypasses.jsonl` (or the path in
`VERIFICATION_BYPASS_LOG`). Events include a timestamp, current commit SHA,
hostname, username, skipped lanes, and the detected pattern. Writes are locked
so concurrent invocations cannot interleave records.

When the runner is used from NEEDLE's pre-commit protocol, add
`--count-bypass` and source the repository's `bypass-detection.sh` helper.
That helper's post-commit hook can then attach the final SHA when Git skips the
pre-commit hook entirely through `git commit --no-verify`.

## Structured reports

Set `--json PATH` or `VERIFICATION_JSON_OUTPUT=true` (with optional
`VERIFICATION_JSON_PATH`) to write a JSON report. It contains top-level totals,
one result for every selected check, and a `failures` array containing only
failed or timed-out checks. Command output is preserved separately as
`stdout` and `stderr`.

## See Also

- [Verification Runner Adoption Guide](adoption-guide.md) - Step-by-step setup for another repository
- [Definition of Done Pattern](definition-of-done-pattern.md) - The motivation and design behind unified verification
- [Definition of Done Adoption Guide](definition-of-done-adoption-guide.md) - How to adopt verification systems in new repos
- [definition-of-done.sh](../scripts/definition-of-done.sh) - The hardcoded implementation for comparison
