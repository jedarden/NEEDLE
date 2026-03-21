# bg-gate: Validation Gates for Bead Closure

## Research Date: 2026-03-20
## Source: https://github.com/antonioc-cl/bg-gate

## What It Is

bg-gate is a minimal Rust CLI that wraps `br` (beads_rust) to enforce validation gates before closing beads. Its core invariant: "a bead can't be closed unless validation passes." It intercepts the close operation, runs configured checks, and only delegates to `br close` if all gates pass.

## How It Works

### Gate Types

**Command Gates**: Execute shell commands via `sh -c "{run}"` in the project root. Pass = exit code 0.
```yaml
gates:
  - name: tests-pass
    type: command
    run: "cargo test --quiet"
    severity: error
```

**Grep-Absent Gates**: Glob-expand file patterns, search for regex. Pass = no matches found.
```yaml
gates:
  - name: no-todo-hacks
    type: grep-absent
    files: "src/**/*.rs"
    pattern: "TODO.*HACK"
    severity: warning
```

### Severity Levels

- **Error**: Blocks closure unconditionally
- **Warning**: Blocks unless `--force` flag is used

### Configuration Waterfall

bg-gate checks for gate definitions in order:
1. `AGENTS.md` sentinel block
2. `CLAUDE.md` sentinel block
3. `.beads/bg.yaml` file
4. Default: no gates (validation always passes)

### CLI Commands

| Command | Purpose |
|---------|---------|
| `beg init` | Create .beads/ structure |
| `beg validate bd-xxx` | Run all gates without closing |
| `beg close bd-xxx --reason "msg"` | Validate then close |
| `beg doctor` | Check setup integrity |

### Structured Output

Exit codes provide diagnostics:
- 0 = success
- 1 = validation failure
- 2 = usage error
- 3 = infrastructure error

JSON output available for programmatic integration.

## Key Design Decisions

1. **Wrapper, not replacement**: bg-gate wraps `br`, does not fork it. All bead operations still go through `br`.

2. **Repository-clean**: Evidence receipts and internal state are gitignored. No dirty working tree from validation runs.

3. **Profile support**: `--profile ci` enables environment-specific severity overrides. Stricter in CI, relaxed locally.

4. **Agent-compatible**: Gates defined in AGENTS.md or CLAUDE.md means AI agents discover validation rules through the same files they already read for project instructions.

## Relevance to NEEDLE

### Direct Applicability

bg-gate solves a problem NEEDLE faces: how to validate that a bead was actually completed correctly before closing it. Currently, NEEDLE evaluates outcomes based on agent exit codes:
- Exit 0 = success -> close bead
- Exit 1 = failure -> release bead

But exit 0 does not mean "the work is correct." An agent can exit successfully while producing broken code. bg-gate adds a validation layer between "agent says done" and "bead is closed."

### Integration Options

**Option 1: Replace `br close` with `beg close`**
NEEDLE's outcome handler for success could invoke `beg close` instead of `br close`. If validation fails, treat it as a failure outcome instead of success.

**Option 2: Run `beg validate` as a pre-close check**
Before closing, NEEDLE runs `beg validate bd-xxx`. On failure, release the bead and log the validation failure. On success, proceed with `br close`.

**Option 3: Build validation into NEEDLE directly**
NEEDLE could implement its own gate system without depending on bg-gate. But bg-gate already exists and handles the edge cases (severity levels, profiles, multiple gate types).

### What bg-gate Validates That NEEDLE Cannot

- Tests pass (`cargo test`, `npm test`, etc.)
- No forbidden patterns in source (TODO hacks, debug prints, hardcoded secrets)
- Lint passes
- Type checks pass
- Custom project-specific checks

### Limitation

bg-gate validates the *repository state*, not the *bead-specific changes*. If worker-alpha completes bead-1 and the tests pass, but worker-bravo simultaneously broke something, bead-1's validation may fail for unrelated reasons. This is a general problem with multi-worker validation, not specific to bg-gate.
