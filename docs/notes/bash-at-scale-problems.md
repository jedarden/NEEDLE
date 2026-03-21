# Bash at Scale Problems

Extracted from the full NEEDLE-deprecated codebase analysis: git log patterns,
bug beads, commit messages, and architecture docs.

## Overview

NEEDLE-deprecated was a ~45,000-line bash application managing concurrent
LLM workers across multiple workspaces. While bash was chosen for its
ubiquity and simplicity, it introduced systemic problems that grew worse
as the codebase scaled.

## Problem 1: No Module System

Bash has no import/module system. NEEDLE used `source` statements to load
dependencies, with `_LOADED` variables to prevent double-sourcing:

```bash
if [[ "${_NEEDLE_CLAIM_LOADED:-}" == "true" ]]; then return 0; fi
_NEEDLE_CLAIM_LOADED=true
source "${NEEDLE_SRC}/lib/output.sh"
source "${NEEDLE_SRC}/bead/select.sh"
```

**Problems caused:**
- Source order matters -- loading A before B fails if A depends on B
- Circular dependencies cause infinite loops or partial loads
- The `_LOADED` guard pattern breaks when concatenated (see
  bundler-build-integrity.md)
- No way to express or validate dependency graphs
- Each module must know the absolute path to its dependencies

**Scale impact:** With 50+ modules, the dependency graph became impossible to
reason about. New modules were added without understanding what they
transitively loaded, leading to subtle initialization order bugs.

## Problem 2: Stdout/Stderr Confusion

Bash functions return values via stdout. Debug logging also goes to stdout by
default. When a function's output is captured in a subshell (`$(func)`),
debug messages corrupt the return value.

**Specific incident:** `_needle_debug` wrote to stdout. When called inside
`_needle_get_claimable_beads`, the debug text corrupted the JSON output,
causing jq to fail and return zero candidates. This was the root cause of
multiple worker starvation alerts (see worker-starvation-lessons.md).

**Fix was simple** (`>&2`), but the bug class is endemic to bash. Every
function that writes diagnostic output and is ever called in a subshell is
vulnerable. There is no compiler or linter that catches this.

## Problem 3: Unbound Variables

Bash's `set -u` (treat unbound variables as errors) is essential for
correctness but creates its own problems:

**Specific incidents:**
- `NEEDLE_QUIET` referenced without `:-` default in welcome.sh and
  update_check.sh, causing unbound variable errors (commit 4ce71e2)
- `NEEDLE_GLOBAL_CONFIG_LOADED_AT` was a typo for `NEEDLE_CONFIG_LOADED_AT`,
  causing the worker loop to exit after ~15 iterations (commit 4ce71e2)
- `_needle_effort` positional args were not optional, causing unbound
  variable crashes when called without arguments (commit 37a0309)

**Pattern:** Variable name typos are silent without `set -u` and crashes
with it. There is no static analysis that catches typos in variable names
across bash files.

## Problem 4: Error Handling is Manual

Bash has no exceptions, no try/catch, no error propagation. Every external
command must have its exit code checked explicitly:

```bash
result=$(br update "$bead_id" --claim 2>&1)
if [[ $? -ne 0 ]]; then
    # handle error
fi
```

**Specific incident:** `br update --blocked-by` is not a valid br flag.
The command silently failed (returned non-zero, output went to /dev/null),
and the calling code did not check. This caused the mitosis-parent re-claim
loop (see claim-race-conditions.md, Race 4).

**Pattern:** In a codebase with hundreds of external command invocations
(br, git, jq, yq, tmux), missing error checks are inevitable. `set -e`
helps but has well-known pitfalls (doesn't work in conditionals, subshells,
pipe components).

## Problem 5: JSON Parsing is Fragile

Bash has no native JSON support. All JSON operations go through `jq`, which
requires:
- Correct quoting of jq expressions inside bash strings
- Proper handling of empty/null/missing fields
- Pipeline error propagation (jq failure in a pipe is swallowed)

**Specific incidents:**
- `br label list` output format changed, breaking jq parsing (3 commits:
  dea6ad2, f9a498a, 9177d63 to fix label parsing)
- `br show --json` missing fields (labels) caused silent guard failures
- Stale variables holding old jq results were used after the source data
  changed (commit 9177d63)

**Pattern:** Every `br` CLI output change required updating jq expressions
across multiple files. There was no schema validation or type checking.

## Problem 6: Concurrency Primitives are Primitive

Bash's concurrency model is processes + signals + files. NEEDLE used:
- `flock` for workspace-level mutexes
- `/dev/shm` files for fast lock state
- Background processes (`&`) for heartbeats
- `trap` for cleanup on exit/signal
- PID files for process tracking

**Specific incidents:**
- Background heartbeat processes were not always cleaned up (zombie processes)
- `flock` on NFS would not work (not relevant here, but limits portability)
- Signal handlers (`trap`) cannot be stacked -- each new trap replaces the
  previous one for that signal
- Process group management for clean shutdown was complex and error-prone

## Problem 7: Testing is Painful

Bash test frameworks exist but are limited:
- No mocking framework -- NEEDLE created its own mock infrastructure by
  replacing `br` with a shell function
- Test isolation requires careful cleanup of global state (exported
  variables, temporary files, running processes)
- No code coverage tools
- No type checking or interface contracts

**Specific incident:** Tests passed against the source tree but failed
against the bundled binary, because the mock infrastructure did not replicate
the concatenation environment.

## Problem 8: 45,000-Line Single File

The bundled `dist/needle` binary was approximately 45,000 lines of bash.
At this scale:
- Grep is the only navigation tool
- Function name collisions are possible (bash has one global namespace)
- Load time is measurable (bash parses the entire file on startup)
- Debugging with `set -x` produces overwhelming output
- Memory usage grows with the number of defined functions

## Problem 9: Current Directory as Implicit State

The `br` CLI uses the current working directory to find its `.beads/`
database. This means every bead operation is implicitly tied to `$PWD`.
Functions that forgot to `cd` to the workspace first operated on the wrong
database (or no database at all).

**Specific incidents:** Four separate bugs in select.sh, claim.sh, the br
wrapper, and explore.sh were caused by operating in the wrong directory.
See worker-starvation-lessons.md, root causes 3 and 4.

## Lessons for the Rewrite

### 1. Use a language with a real module system

Imports, namespaces, and dependency resolution should be handled by the
language, not by convention.

### 2. Use a language with structured error handling

Exceptions, Result types, or at minimum compile-time checking that errors
are handled. Silent failures from unchecked exit codes were the single
largest bug category.

### 3. Use a language with native JSON support

Parsing CLI output through jq pipelines is fragile. A language with native
JSON deserialization (with schema validation) eliminates an entire class of
bugs.

### 4. Use explicit parameters, not implicit state

Workspace paths, database connections, and configuration should be passed
explicitly through function parameters or dependency injection, not derived
from `$PWD` or global variables.

### 5. Separate logging from return values

A logging system that writes to a dedicated channel (not stdout) prevents
the debug-output-corrupting-return-values class of bugs entirely.

### 6. Static analysis catches most of these bugs

A compiled language (or a dynamically-typed language with a good linter)
would catch: undefined functions, unbound variables, unused variables, type
mismatches in JSON parsing, and missing error handling. Bash linters
(shellcheck) catch some of these but miss many.

### 7. Concurrent code needs proper primitives

Mutexes, channels, async/await, or actor models are all better than
flock + PID files + trap handlers. The concurrency bugs in NEEDLE were
caused by the primitiveness of bash's process model.

## Source Evidence

- Commits `4ce71e2`: 6 simultaneous build/runtime issues (unbound vars, typos, path doubling)
- Commit `37a0309`: unbound variable crash in effort module
- Commits `dea6ad2`, `f9a498a`, `9177d63`: label parsing bugs across 3 fixes
- Commit `06387e0`: per-workspace flock for thundering herd
- Worker starvation alerts (16 beads): stdout pollution, wrong directory
- Bead `nd-096dsr`: missing functions from manual module list
