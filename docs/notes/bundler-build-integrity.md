# Bundler and Build Integrity

Extracted from git log commit messages for build-related fixes and the
bead nd-096dsr (missing lock functions).

## The Problem

NEEDLE is written as ~50+ individual bash source files that are concatenated
into a single self-contained binary (`dist/needle`) by a build script. This
concatenation process introduced multiple classes of bugs that were not
present in the source tree.

## Bug 1: Missing Modules in Build (35+ modules)

**What happened (commit 9c28436):** The bundled binary was missing 35 modules
including all CLI commands, the lock system, quality checks, JSON utilities,
and diagnostic tools. Workers could not run any subcommands.

**Root cause:** The build script maintained a **manual list** of modules to
include (`MODULES=(...)`). New files added to `src/` were not automatically
picked up. Nobody updated the list when adding new modules.

**Fix:** Added all missing modules to the MODULES list. But this was a
recurring problem -- every new module required a manual build list update.

## Bug 2: Source Guards Breaking the Bundled Binary (v0.13.0)

**What happened:** Every module had a source guard pattern:

```bash
if [[ "${_NEEDLE_FOO_LOADED:-}" == "true" ]]; then
    return 0
fi
_NEEDLE_FOO_LOADED=true
```

The build script set all `_NEEDLE_*_LOADED=true` variables at the top of the
bundled binary (to prevent double-loading). But it did not strip the guards
from the module bodies. Result: every module's guard fired immediately and
skipped all function definitions.

**Symptom:** `_needle_verify_bead: command not found` -- workers executed
beads but could not close them because the verify function was never defined.

**Fix:** Build script now strips re-source guard blocks (both `return 0` and
`else` patterns).

## Bug 3: Bare `return 0` at Top Level (commit 4a35e95)

**What happened:** `src/bead/verify.sh` had a top-level `return 0` as its
source guard. In the source tree, this exited the `source` command harmlessly.
In the concatenated binary, there was no enclosing function -- `return 0`
terminated the entire script at that point, preventing all subsequent modules
from loading.

**Fix:** Replaced the bare `return 0` with an `if/else` block that only
skips the module body, not the rest of the script.

## Bug 4: Invalid Bash Syntax in Concatenation (nd-3b23)

**What happened (commit 06aed0e):** The build script produced a file that
failed `bash -n` syntax checking. Empty `if-then-else` blocks (where the
body was only shellcheck directives) produced syntax errors when the
directives were stripped.

**Fix:** Build script now handles empty if-then-else blocks by inserting
`:` (no-op) when comment-only bodies are stripped.

## Bug 5: Missing Lock Functions (nd-096dsr)

**What happened:** The functions `_needle_acquire_claim_lock` and
`_needle_release_claim_lock` were called in 7+ locations but never defined.
All workers failed on every workspace claim attempt with `command not found`.

**Root cause:** The lock implementation file (`src/lib/locks.sh`) existed in
the source tree but was not added to the build script's module list. The
functions were added to call sites (claim, release, pluck, mend, explore)
but the implementation was never bundled.

**Impact:** All 4 workers stuck in a dead loop, cycling through every
workspace, failing to claim any, producing zero output.

## Bug 6: bundle.sh vs build.sh Confusion (commit 6420e0f)

**What happened:** Two build scripts existed: `scripts/bundle.sh` (produced a
1106-line stub with `source` statements) and `scripts/build.sh` (produced a
proper ~45K-line self-contained binary). A commit used `bundle.sh` instead of
`build.sh`, deploying a stub that tried to source files that did not exist at
the install location.

**Fix:** Standardized on `build.sh` and documented that `bundle.sh` is for
development only.

## Bug 7: Missing Stream-Parser in Build (nd-v8ib)

**What happened:** The `stream-parser.sh` file (used for agent output
formatting) was not embedded in the bundled binary. Workers could not parse
agent output, breaking telemetry extraction and heartbeat integration.

**Fix:** Updated build.sh to embed stream-parser as
`_NEEDLE_EMBEDDED_STREAM_PARSER` and extract it alongside YAML agent configs
on first run.

## Bug 8: Constants Version Mismatch (commit 291eb2f)

**What happened:** The version string in `src/lib/constants.sh` was not
updated when the build version was bumped. The binary reported the wrong
version, and version-check logic behaved incorrectly.

**Fix:** Added constants.sh version sync to the release workflow.

## The Pattern

Every one of these bugs shares a root cause: **the build process had no
automated validation**. The build script was a concatenation tool, not a
build system. It did not verify:

- That all referenced functions were defined in the output
- That the output passed syntax checking
- That all source files were included
- That no top-level statements would break concatenation
- That embedded resources were present

## Lessons for the Rewrite

### 1. Auto-discover source files

Never maintain a manual module list. The build system should glob all source
files in the project tree, or use a manifest that is validated against the
filesystem.

### 2. Validate the build output

At minimum:
- `bash -n output.sh` (syntax check)
- Grep for all function calls and verify each has a matching definition
- Run a smoke test that exercises major code paths

### 3. Source guards don't work in concatenated scripts

The `_LOADED` pattern is designed for `source` semantics. In a concatenated
binary, either strip the guards entirely (since double-loading is impossible)
or use a different mechanism (e.g., `declare -F function_name` checks).

### 4. Avoid top-level `return` in library files

`return` at the top level of a file means different things depending on
whether the file is sourced or concatenated. Use `if/else` blocks instead.

### 5. One build script, not two

Having both `bundle.sh` and `build.sh` created confusion about which to use.
A single build pipeline with clear development vs. production modes is safer.

### 6. Consider a real build system or a different language

Bash concatenation is inherently fragile. The rewrite should either use a
language with proper module/import systems, or use a build tool that
understands bash semantics (e.g., checking that all function references
resolve).

### 7. Test the bundled binary, not just the source

Integration tests that ran against the source tree would pass, but the
bundled binary would fail. Tests must run against the actual deployment
artifact.

## Source Evidence

- Commit `9c28436`: complete bundled binary with all modules (35 missing)
- Commit `4a35e95`: replace bare return in verify.sh guard
- Commit `06aed0e`: fix build.sh concatenation for valid syntax
- Commit `6420e0f`: use build.sh instead of bundle.sh
- Commit `fbb558b`: add missing `_needle_is_strand_enabled` function
- Commit `4ce71e2`: resolve 6 simultaneous build/runtime issues
- Bead `nd-096dsr`: missing lock functions breaking all workers
- Bead `nd-3b23`: build.sh concatenation producing invalid bash
