# Definition of Done: Unified Verification Mechanism

## Problem Statement

The same question — "is this work acceptable?" — is currently answered in four disconnected places that have drifted apart:

1. **Pre-commit hook** — `commitgraph/.githooks/pre-commit` runs `go vet ./...` + `go test ./...`. Bypassable, and bypassed: 23 commits landed with `--no-verify` while it was failing.
2. **CI verify step** — `needle-ci` runs fmt, clippy, check, `cargo test --lib`, and exactly one named integration test. 53 of NEEDLE's 54 test files never run. Async and advisory; red for 6+ consecutive runs without anyone noticing.
3. **NEEDLE gate** — `config.gates` / `config.verification`. Configured in only 5 of 71 workspaces. NEEDLE's own gate is `verify-shipped-commit.sh`, which checks that a commit exists — it never compiles, formats, or lints anything. NO workspace anywhere runs `cargo check` or `go build` as a gate.
4. **The agent itself** — whatever the model decides "done" means this dispatch.

## Solution: One Declared Command Per Repo

**Core principle:** A single declared command that all surfaces invoke identically.

### Declaration Location

Each repo declares its definition of done in a standardized location:

```
.verify/verify.sh          # The definition of done script
.verify/config.json        # Optional: declarative config for generated scripts
```

The `.verify/` directory is:
- Version-controlled with the repo
- Discoverable by all tooling (hooks, CI, NEEDLE)
- Language-agnostic (any repo can have one)

### Script Contract

The `verify.sh` script **MUST**:

1. **Accept arguments** for lane selection:
   ```bash
   .verify/verify.sh [--fast|--slow|--all]
   ```
   - `--fast`: Fast lane only (seconds, local) — formatting, linting, type checking
   - `--slow`: Slow lane only (may submit to CI) — full test suite
   - `--all` (default): Both lanes

2. **Aggregate failures** — never abort on first error:
   ```bash
   # Must collect ALL failures and report them together
   # Exit with count of failures
   ```

3. **Return structured exit codes**:
   - `0`: All checks passed
   - `1-N`: Number of failed checks (so tooling can distinguish "one formatting issue" from "everything broken")

4. **Produce human-readable output**:
   - Clear section headers (FAST LANE, SLOW LANE)
   - Per-check summaries with pass/fail status
   - Aggregate count at the end

### Example Implementation

```bash
#!/usr/bin/env bash
set -o pipefail

FAST_LANE_FAILED=0
SLOW_LANE_FAILED=0

run_check() {
    local name="$1"
    local command="$2"
    
    echo "[$name] Running..."
    if eval "$command" >/dev/null 2>&1; then
        echo "[$name] ✓ PASSED"
        return 0
    else
        echo "[$name] ✗ FAILED"
        return 1
    fi
}

# Fast lane (seconds, local)
if [[ "$1" != "--slow" ]]; then
    echo "=== FAST LANE ==="
    run_check "fmt" "cargo fmt --check" || ((FAST_LANE_FAILED++))
    run_check "clippy" "cargo clippy --all-targets -- -D warnings" || ((FAST_LANE_FAILED++))
    run_check "check" "cargo check" || ((FAST_LANE_FAILED++))
    echo "Fast lane failures: $FAST_LANE_FAILED"
fi

# Slow lane (test suite, may go to CI)
if [[ "$1" != "--fast" ]]; then
    echo "=== SLOW LANE ==="
    run_check "unit tests" "cargo test --lib" || ((SLOW_LANE_FAILED++))
    run_check "integration tests" "cargo test --test integration_tests" || ((SLOW_LANE_FAILED++))
    echo "Slow lane failures: $SLOW_LANE_FAILED"
fi

TOTAL_FAILED=$((FAST_LANE_FAILED + SLOW_LANE_FAILED))
echo "Total failures: $TOTAL_FAILED"
exit $TOTAL_FAILED
```

## Integration Points

### 1. Pre-commit Hook

```githooks
#!/usr/bin/env bash
# .githooks/pre-commit

VERIFY_SCRIPT=".verify/verify.sh"
BYPASS_LOG=".verify/bypasses.log"

if [[ -f "$VERIFY_SCRIPT" ]]; then
    if ! "$VERIFY_SCRIPT" --fast; then
        echo "Pre-commit checks failed. Use 'git commit --no-verify' to bypass."
        exit 1
    fi
else
    echo "No verify script found — committing without verification."
fi
```

**Bypass counting:**

```githooks
# In post-commit hook (to count bypasses)
if git log -1 --format=%s | grep -q "\\[skip verify\\]"; then
    echo "$(git rev-parse HEAD) $(date -Iseconds)" >> "$BYPASS_LOG"
fi
```

### 2. CI Verify Step

The CI workflow calls the same script:

```yaml
- name: Verify
  run: .verify/verify.sh --all
```

CI runs **both lanes** because it has the resources and time budget.

### 3. NEEDLE Gate

```yaml
# .needle.yaml
verification:
  command: [".verify/verify.sh", "--fast"]
  on_failure: retry_with_human
```

The agent-facing gate uses **only the fast lane** because:
- It must complete in seconds, not minutes
- Agents work on incrementally-valid code (fix fmt → claim → fix clippy → claim)
- The slow lane runs asynchronously in CI

### 4. The Agent Itself

Agents are instructed (via system prompt or workspace instructions) that "done" means:

> "Your work is complete when `.verify/verify.sh --fast` passes with zero failures."

This gives the agent a concrete, executable definition of done — not a subjective judgment.

## Migration Path

### Phase 1: Clean Existing Debt

**Before** wiring the gate, the repo must pass its own definition of done.

Current debt:
- NEEDLE: 52 fmt diffs dirty across 7 committed files (needle-3653fee9)
- commitgraph: `go test` failing (commitgr-44a76623)

**Action:** Create debt beads, fix the issues, close beads.

### Phase 2: Implement verify.sh

Create `.verify/verify.sh` with:
- Fast lane that reflects current best practices
- Slow lane that matches what CI should run
- Aggregated failure reporting

### Phase 3: Wire Surfaces

1. **Pre-commit hook:** Update `.githooks/pre-commit` to call `.verify/verify.sh --fast`
2. **CI:** Update `needle-ci` WorkflowTemplate to call `.verify/verify.sh --all`
3. **NEEDLE gate:** Update `.needle.yaml` `verification.command` to `[.verify/verify.sh, --fast]`

### Phase 4: Enable Enforcement

Once all three surfaces invoke the same script:
- Remove `--no-verify` bypass option from hook (make it hard fail)
- Make CI verification blocking (currently advisory)
- Count bypasses in `.verify/bypasses.log`

## Benefits

1. **Single source of truth** — One script defines what "done" means.
2. **Drift-proof by construction** — All surfaces invoke the same command.
3. **Fast feedback** — Agents get seconds-scale validation; CI gets comprehensive validation.
4. **Aggregate failures** — One dispatch teaches the agent about all issues at once.
5. **Counted bypasses** — We know when quality gates are being bypassed.
6. **Language-agnostic** — Works for Rust, Go, TypeScript, Python, etc.

## Rollout Strategy

1. **Start with NEEDLE itself** — Dogfood the mechanism in the repo that implements NEEDLE.
2. **Expand to high-value repos** — commitgraph, tradegraph, SEAM (repos with active bead work).
3. **Template and documentation** — Create copy-paste examples for common language setups.
4. **Gradual rollout** — One repo at a time, behind debt cleanup.

## Success Criteria

A repo has a unified definition of done when:
- [ ] `.verify/verify.sh` exists and is executable
- [ ] Pre-commit hook calls it with `--fast`
- [ ] CI workflow calls it with `--all`
- [ ] NEEDLE verification gate calls it with `--fast`
- [ ] The script aggregates failures and exits with count
- [ ] Bypasses are logged to `.verify/bypasses.log`
- [ ] The repo passes its own verification (no existing debt)
