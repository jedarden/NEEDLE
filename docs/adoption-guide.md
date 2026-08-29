# Verification Runner Adoption Guide

This guide explains how to add NEEDLE's configurable verification runner to
another repository. The runner keeps the checks in YAML and executes them in
two cost-based lanes:

- **Fast**: formatting, linting, type checking, and other checks suitable for a
  pre-commit hook.
- **Slow**: unit, integration, documentation, and other checks that belong in
  CI or a release gate.

The runner executes every check in the selected lane and reports all failures
together. It returns zero only when every non-optional check passes.

This document covers `scripts/verification-runner.sh`, the configurable YAML
runner. NEEDLE also contains `scripts/definition-of-done.sh`, a separate,
hard-coded gate used by NEEDLE's own pre-commit hook and NEEDLE gate. The
configurable runner supports explicit skip detection itself; when paired with
NEEDLE's hook protocol, `--count-bypass` also records pre-commit state for the
shared post-commit logger.

## Quick start

From the root of the repository you are adopting:

```bash
mkdir -p scripts .verification

# Copy these two files from a pinned NEEDLE revision or release:
cp /path/to/NEEDLE/scripts/verification-runner.sh scripts/verification-runner.sh
cp /path/to/NEEDLE/docs/templates/verification-config.yaml \
  .verification/config.yaml
chmod +x scripts/verification-runner.sh
```

Install the runner's host dependencies and confirm they are available:

```bash
bash --version
git --version
yq --version       # mikefarah/yq, which supports `yq eval`
jq --version
timeout --version  # GNU timeout, used for per-check limits
```

Edit `.verification/config.yaml` so each command matches the repository, then
validate the configuration without running checks:

```bash
./scripts/verification-runner.sh --dry-run --fast
```

Run the lanes directly:

```bash
./scripts/verification-runner.sh --fast  # local/pre-commit checks
./scripts/verification-runner.sh --slow  # comprehensive checks
./scripts/verification-runner.sh --all   # both lanes (the default)
```

The script finds configuration in this order when `--config` is omitted:

1. `.verification/config.yaml`
2. `definition-of-done.yaml`
3. `.verification/config.yml`
4. `definition-of-done.yml`

Use `--config path/to/file.yaml` to select a different file explicitly. The
runner changes to the Git repository root before executing checks, so commands
may use repository-relative paths regardless of the caller's initial directory.

## Configuration

Copy [`templates/verification-config.yaml`](templates/verification-config.yaml)
as the starting point. The top-level `version` field identifies the config
format. `fast_lane` and `slow_lane` are lists of checks; an empty or absent lane
has nothing to run, so a useful adoption normally defines both.

Each check has the following fields:

| Field | Required | Meaning |
| --- | --- | --- |
| `name` | Yes | Human-readable name shown in the report. |
| `command` | Yes | Executable to invoke; it must be on `PATH` or be a repository-relative path. |
| `args` | No | YAML list of command arguments. An omitted list behaves like an empty list. |
| `timeout` | No | Maximum runtime in seconds. The runner defaults to 60 seconds when it is omitted. |
| `description` | No | Human-readable explanation for maintainers. |
| `allow_failure` | No | Set to `true` for a warning-only check; defaults to `false`. |
| `environment` | No | List of `KEY=value` strings exported only while that check runs. |

The runner passes each `args` list item as one argv element, so arguments with
spaces, quotes, or shell metacharacters do not get reinterpreted by a shell.
Pipelines and redirections are intentionally not YAML commands; put those in a
small checked-in wrapper script and call that wrapper instead.

An optional environment example looks like this:

```yaml
environment: ["CARGO_TERM_COLOR=always", "RUST_BACKTRACE=1"]
```

Use the sequence form above, not a YAML mapping. Environment values may contain
secrets only when the check's execution environment already supplies them; do
not commit credentials to the configuration file.

### Choosing lanes

The fast lane should be deterministic and short enough to run before every
commit. Typical entries include:

```yaml
fast_lane:
  - name: "Format check"
    command: "npm"
    args: ["run", "format:check"]
    timeout: 60

  - name: "Lint"
    command: "npm"
    args: ["run", "lint"]
    timeout: 120
```

Put the full test suite, integration tests requiring services, and expensive
builds in `slow_lane`:

```yaml
slow_lane:
  - name: "Unit tests"
    command: "npm"
    args: ["test", "--", "--runInBand"]
    timeout: 600

  - name: "Production build"
    command: "npm"
    args: ["run", "build"]
    timeout: 600
```

Choose timeouts from observed runtime plus headroom. A timeout is a failure,
not a warning, unless `allow_failure: true` is set. A check with
`allow_failure: true` still appears in the output, but does not make the
overall run fail.

## Running in CI

The CI verify job should run the complete definition of done from a clean
checkout:

```bash
set -euo pipefail
./scripts/verification-runner.sh --all --verbose
```

Make sure CI installs the same `yq`, `jq`, and `timeout` implementations used
locally. Pin the runner source to a known NEEDLE revision or release and review
runner changes as part of dependency updates. If the CI system has a native
timeout, keep the per-check timeout as well; the latter identifies which check
failed and prevents one command from consuming the whole job.

For machine-readable consumers, set the JSON output variables:

```bash
VERIFICATION_JSON_OUTPUT=true \
VERIFICATION_JSON_PATH=artifacts/verification-results.json \
  ./scripts/verification-runner.sh --all
```

Create the output directory in the CI job and publish the file as an artifact.
Do not commit generated results.

## Integrating with an existing pre-commit hook

Keep the hook's existing setup and checks. Add one invocation of the fast lane
after those checks, or replace only the old equivalent checks once the YAML
configuration has been verified:

```bash
#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

# Existing repository-specific checks remain here.
# ./tools/check-generated-files.sh

echo "=== Running verification runner (fast lane) ==="
./scripts/verification-runner.sh --fast
```

For a tracked hook in `.githooks/`, make it executable and select that hook
directory locally:

```bash
chmod +x .githooks/pre-commit
git config --local core.hooksPath .githooks
```

If the repository uses Husky, pre-commit, Lefthook, or another hook manager,
add `./scripts/verification-runner.sh --fast` as one command in that manager
instead of replacing its generated hook. Do not run `--all` from pre-commit;
slow checks belong in CI.

The runner recognizes `--no-verify` and the explicit skip environments
`SKIP_CHECKS=1`, `VERIFICATION_SKIP=1`, and
`NEEDLE_SKIP_VERIFICATION=1`. It records a structured event to
`.beads/bypasses.jsonl`, or to `VERIFICATION_BYPASS_LOG` when set. If the
repository uses NEEDLE's pre-commit/post-commit logger, add `--count-bypass`
to allow the logger to attach the final commit SHA.

## Optional NEEDLE gate

A repository managed by NEEDLE can use the fast lane as its validation gate:

```yaml
# .needle.yaml
gates:
  - type: command
    commands:
      - scripts/verification-runner.sh --fast
```

Run the command directly and make sure the fast lane is reliably green before
enabling a blocking gate. The gate runs from the workspace, so keep the script,
config, and all referenced wrapper scripts in the repository. If the project
uses NEEDLE's bypass-aware definition-of-done implementation instead, point the
gate at `scripts/definition-of-done.sh --fast` as NEEDLE does.

## NEEDLE reference setup

NEEDLE is the reference repository and provides both runner styles:

| Purpose | NEEDLE path or command |
| --- | --- |
| Configurable runner | `scripts/verification-runner.sh` |
| Configurable runner config | `.verification/config.yaml` |
| Fast smoke test | `./scripts/verification-runner.sh --config .verification/config.yaml --fast` |
| YAML dry run | `./scripts/verification-runner.sh --config .verification/config.yaml --dry-run --fast` |
| Enforced pre-commit hook | `.githooks/pre-commit` |
| NEEDLE's bypass-aware gate | `scripts/definition-of-done.sh --fast` |
| NEEDLE gate declaration | `.needle.yaml`, under `gates` |

The committed configurable example currently puts Rust formatting, Clippy, and
compilation in its fast lane. Its slow lane demonstrates named core integration,
bead-rs, and installer checks. Re-run the dry run after changing the config, then
run the fast lane as the adoption smoke test:

```bash
cd /home/coding/NEEDLE
./scripts/verification-runner.sh --config .verification/config.yaml --dry-run --fast
./scripts/verification-runner.sh --config .verification/config.yaml --fast
```

NEEDLE's mandatory surfaces intentionally use the specialized
`definition-of-done.sh`: the pre-commit hook invokes it with
`--fast --count-bypass`, the `.needle.yaml` gate invokes its fast lane, and CI
uses its all-lanes mode. This is a reference for wiring the same command into
multiple surfaces; adopters using the configurable runner should substitute
`verification-runner.sh` and omit the bypass-specific flag.

## Troubleshooting

### `No configuration file found`

Run the command from inside the repository, create `.verification/config.yaml`,
or pass the exact path with `--config`. Check the spelling and case of the
filename. The runner does not search arbitrary directories.

### `yq is required for YAML parsing but not found`

Install mikefarah/yq and ensure the executable is on `PATH`. This runner uses
the `yq eval` command syntax; a different YAML tool with an incompatible CLI is
not a drop-in replacement.

### Arguments are not passed as expected

The runner passes each `args` list item as one argv element. Pipelines,
redirections, and substitutions are intentionally not YAML commands; put those
in a checked-in wrapper script and configure that script as the check command.

### A check fails with exit code 124

The check exceeded its configured timeout. Run that command by itself to
measure it, then either fix the slow behavior or increase `timeout` with enough
headroom. Do not increase every timeout blindly: a bounded check prevents hung
processes from blocking CI.

### The report says `Failed: 0`, but no checks ran

The selected lane is empty or missing. Add checks to `fast_lane` or `slow_lane`,
and use `--dry-run --verbose` to confirm what the runner will execute.

### The pre-commit hook does not run

Check the hook path and executable bit:

```bash
git config --get core.hooksPath
git ls-files --stage .githooks/pre-commit
```

The first command should point at the directory containing the hook, and the
mode shown by the second should include `100755`. Also verify that the hook
calls the runner from the repository root.

### The NEEDLE gate fails while the command passes locally

Run the exact gate command from a clean checkout. Compare `PATH`, tool versions,
environment variables, and the selected config file. Avoid relying on shell
aliases or untracked wrapper scripts; NEEDLE sees only the committed workspace.

### I need bypass logging

The generic runner logs explicit skip requests. To record a commit skipped
entirely by `git commit --no-verify`, retain a post-commit hook/logger because a
process that was never started cannot observe that Git flag. Do not silently
treat that commit as a successful verification result.

## Adoption checklist

- [ ] Copy a pinned `verification-runner.sh` and make it executable.
- [ ] Install and verify Bash, Git, `yq`, `jq`, and `timeout`.
- [ ] Copy and customize `.verification/config.yaml`.
- [ ] Keep fast checks short and deterministic; put expensive checks in slow.
- [ ] Run `--dry-run --fast`, then run `--fast` successfully.
- [ ] Add the fast lane to the existing pre-commit integration.
- [ ] Add `--all` to the CI verify job.
- [ ] Add a NEEDLE gate only after the fast lane is consistently green.
- [ ] Decide explicitly how `--no-verify` events are audited.
- [ ] Document any repository-specific wrapper commands and required services.
