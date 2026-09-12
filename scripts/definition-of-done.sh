#!/usr/bin/env bash
# Unified Definition of Done for NEEDLE
#
# This script is the single source of truth for "is this work acceptable?"
# It is invoked identically by:
#   - Pre-commit hook (fast lane only, with --count-bypass)
#   - CI verify step (both fast and slow lanes)
#   - NEEDLE validation gate (fast lane only)
#
# Lanes:
#   - Fast: fmt, clippy (seconds, run locally under cgroup)
#   - Slow: unit and core strand integration targets
#
# Behavior: Aggregates all failures rather than aborting on first.
# Returns non-zero if ANY check fails, with all failures reported.
#
# Usage:
#   scripts/definition-of-done.sh [--fast|--slow|--all] [--count-bypass]
#                                 [--changed-only] [--gate] [--target <name>]
#
# Flags:
#   --fast          Run fast lane only (default for NEEDLE gate)
#   --slow          Run slow lane only (tests)
#   --all           Run both lanes (default for CI)
#   --count-bypass   Track the pre-commit result so post-commit can detect
#                    commits made with --no-verify
#   --changed-only   Hold this commit responsible only for the paths it stages.
#                    A problem in a file this commit does not touch is reported
#                    loudly but does not block. See "Attribution" below.
#   --gate           Short-circuit: a failed fast lane returns before the slow
#                    lane starts, so CI never compiles test targets for code
#                    already known to be rejected. Default behaviour is
#                    unchanged -- aggregation is right for a human at a
#                    terminal, who wants every failure in one pass -- because
#                    CI is the only caller that passes this. See "Gating" below.
#   --target <name>  Run exactly one slow-lane target instead of all of them.
#                    Implies the slow lane. Valid names are printed by
#                    needle_slow_targets(). CI uses this to give each test
#                    target its own pod. See "Per-target runs" below.
#                    Both the space form and the equals form are accepted;
#                    needle-workflowtemplate.yml passes the equals form, so
#                    that one is the one CI actually exercises.

set -euo pipefail

# Script directory for path resolution
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$REPO_ROOT" ]]; then
  # Not inside a git work tree. NEEDLE's clean gate (ADR-020) runs this script
  # from a `git archive HEAD` extraction, which has no .git directory; there the
  # extraction root is the script's parent. Before 2026-09-02 this line was a
  # hard `git rev-parse` and every clean-mode gate died with "not a git
  # repository" before checking anything (42 failures on 2026-09-02 alone).
  REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
fi
cd "$REPO_ROOT"

# Default to fast lane
LANE="fast"
COUNT_BYPASS=false
CHANGED_ONLY=false
GATE=false
SLOW_TARGET=""
NEEDLE_BYPASS_ARGUMENT=""

# Parse arguments
while [[ $# -gt 0 ]]; do
  case $1 in
    --fast)
      LANE="fast"
      shift
      ;;
    --slow)
      LANE="slow"
      shift
      ;;
    --all)
      LANE="all"
      shift
      ;;
    --count-bypass)
      COUNT_BYPASS=true
      shift
      ;;
    --changed-only)
      CHANGED_ONLY=true
      shift
      ;;
    --gate)
      GATE=true
      shift
      ;;
    --target)
      [[ $# -ge 2 ]] || { echo "Error: --target requires a name" >&2; exit 1; }
      SLOW_TARGET="$2"
      shift 2
      ;;
    --target=*)
      # The equals form is what needle-workflowtemplate.yml passes
      # (`--target={{inputs.parameters.target}}`), and Argo renders it
      # verbatim — an unparsed equals form failed every slow-lane pod in CI
      # with "Unknown argument" while the fast lane stayed green.
      [[ -n "${1#--target=}" ]] || { echo "Error: --target requires a name" >&2; exit 1; }
      SLOW_TARGET="${1#--target=}"
      shift
      ;;
    --no-verify)
      NEEDLE_BYPASS_ARGUMENT="--no-verify"
      shift
      ;;
    *)
      echo "Error: Unknown argument: $1" >&2
      echo "Usage: $0 [--fast|--slow|--all] [--count-bypass] [--changed-only] [--gate] [--target <name>] [--no-verify]" >&2
      exit 1
      ;;
  esac
done

# ── Slow-lane target table ───────────────────────────────────────────────────
#
# The slow lane's targets are named in ONE place so the build step and the run
# step can never disagree about what counts as a target, and so `--target` has
# something to validate against.
#
# needle_slow_targets lists every accepted `--target` value, `installer`
# included. needle_cargo_selector maps a name to the cargo selector that runs
# it, and fails for anything else -- including `installer`, which is a shell
# suite with no cargo target. Both are extracted and unit-tested by
# tests/dod-modes/run.sh rather than copied, so they cannot drift from what
# actually runs.
needle_slow_targets() {
  printf '%s\n' lib integration_tests p2_integration_tests \
    p3_integration_tests real_br_integration_tests installer
}

needle_cargo_selector() {
  case "$1" in
    lib)                       printf '%s\n' --lib ;;
    integration_tests)         printf '%s\n' --test integration_tests ;;
    p2_integration_tests)      printf '%s\n' --test p2_integration_tests ;;
    p3_integration_tests)      printf '%s\n' --test p3_integration_tests ;;
    real_br_integration_tests) printf '%s\n' --test real_br_integration_tests ;;
    *)                         return 1 ;;
  esac
}

# --target runs exactly one slow-lane check, so it implies the slow lane even
# though `--fast` is the default.
if [[ -n "$SLOW_TARGET" ]]; then
  LANE="slow"
  if ! needle_slow_targets | grep -qx -- "$SLOW_TARGET"; then
    echo "Error: unknown --target '$SLOW_TARGET'. Valid targets:" >&2
    needle_slow_targets | sed 's/^/  /' >&2
    exit 1
  fi
fi

# Which cargo targets this invocation covers. --target installer names no
# cargo target at all, so this yields nothing rather than every target.
selected_cargo_targets() {
  if [[ -n "$SLOW_TARGET" && "$SLOW_TARGET" != "installer" ]]; then
    printf '%s\n' "$SLOW_TARGET"
    return 0
  fi
  if [[ "$SLOW_TARGET" == "installer" ]]; then
    return 0
  fi
  needle_slow_targets | grep -vx installer
}

# Bypass detection.  A pre-commit invocation writes a marker keyed by the
# candidate tree; post-commit attaches the final commit SHA.  A direct script
# invocation has no future commit to attach, so it is logged immediately.
source "$SCRIPT_DIR/bypass-detection.sh"
BYPASS_PATTERN=""
if [[ -n "$NEEDLE_BYPASS_ARGUMENT" ]]; then
  BYPASS_PATTERN="$NEEDLE_BYPASS_ARGUMENT"
elif needle_bypass_requested; then
  BYPASS_PATTERN="$(needle_bypass_pattern)"
fi

if [[ -n "$BYPASS_PATTERN" ]]; then
  BYPASS_LANES="$(needle_lanes_csv "$LANE")"
  needle_warn_bypass "$BYPASS_PATTERN" "$BYPASS_LANES"
  if [[ "${NEEDLE_PRE_COMMIT:-}" == 1 ]]; then
    if ! needle_mark_bypass "$BYPASS_LANES" "$BYPASS_PATTERN"; then
      echo "ERROR: Could not record the pending verification bypass." >&2
      exit 1
    fi
  else
    bypass_record="$(needle_json_event "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$(needle_current_commit)" "$BYPASS_LANES" "$BYPASS_PATTERN" "Verification was explicitly skipped" "$(pwd -P)")"
    if ! needle_append_bypass_event "$bypass_record"; then
      echo "ERROR: Could not record the verification bypass." >&2
      exit 1
    fi
  fi
  exit 0
fi

# Failure tracking
declare -a FAILURES=()
declare -a CHECKS=()
# Checks that failed on files this commit does not stage (--changed-only).
declare -a PREEXISTING=()
# Slow-lane scratch directories are registered in this shell. Do not call
# lane_tmp through command substitution: that runs the function in a subshell
# and silently discards the LANE_TMPS append, leaking the directory at exit.
declare -a LANE_TMPS=()

lane_tmp() {
  local label="$1" destination="$2"
  local root="${DOD_TMP_ROOT:-/var/tmp}"
  local directory

  [[ -d "$root" && -w "$root" ]] || root="/tmp"
  directory="$(mktemp -d "$root/needle-dod-$label.XXXXXX")"
  LANE_TMPS+=("$directory")
  printf -v "$destination" '%s' "$directory"
}

cleanup_lane_tmps() {
  [[ "${#LANE_TMPS[@]}" -eq 0 ]] || rm -rf "${LANE_TMPS[@]}" 2>/dev/null || true
  # Remove the poison below only if this run created it. Pre-existing litter
  # stays: deleting host state we did not create is worse than leaving it, and
  # the lanes are required to pass with it present anyway.
  if [[ "${POISON_WE_CREATED:-0}" == 1 ]]; then
    rm -rf "$POISON_DIR" 2>/dev/null || true
  fi
}

needle_now_ms() {
  local now
  now="$(date +%s%3N)"
  if [[ "$now" =~ ^[0-9]+$ ]]; then
    printf '%s\n' "$now"
  else
    # GNU date supplies milliseconds in CI. Retain a numeric, second-resolution
    # fallback for platforms whose date prints the %3N directive literally.
    printf '%s000\n' "$(date +%s)"
  fi
}

needle_duration_event() {
  local phase="$1" target="$2" duration_ms="$3" status="$4" exit_code="$5"
  printf 'NEEDLE_DOD_TIMING {"phase":"%s","target":"%s","duration_ms":%s,"status":"%s","exit_code":%s}\n' \
    "$phase" "$target" "$duration_ms" "$status" "$exit_code"
}

# Helper to run a check and record failure
# Kill anything the finished check left behind. A leftover inherits the check's
# stdout, so a process whose fd 1 still points at this check's log is by
# definition one of its orphans -- which makes this precise: it cannot match an
# unrelated process, another check, or a concurrent CI run (each gets its own
# mktemp log).
reap_orphans() {
  local log="$1" target pid fd1
  [[ -d /proc ]] || return 0          # /proc scan is Linux-only; no-op elsewhere
  target="$(readlink -f "$log" 2>/dev/null)" || return 0
  [[ -n "$target" ]] || return 0

  for p in /proc/[0-9]*; do
    pid="${p#/proc/}"
    [[ "$pid" == "$$" ]] && continue
    fd1="$(readlink -f "$p/fd/1" 2>/dev/null)" || continue
    if [[ "$fd1" == "$target" ]]; then
      kill -9 "$pid" 2>/dev/null || true
      echo "  reaped orphaned pid $pid (still writing to this check's output)"
    fi
  done
}

run_check() {
  local name="$1"
  shift
  CHECKS+=("$name")

  echo "Running: $name..."

  # Capture to a FILE, never a command substitution.
  #
  # `output=$(cmd)` reads the pipe until every WRITER closes it, not until the
  # command exits. `timeout N cargo test ...` signals the process group, but a
  # test binary that installs a SIGTERM handler (NEEDLE's worker does, for
  # graceful shutdown) survives it, is reparented to init, and keeps fd 1/2 on
  # that pipe. The read then never returns: the timeout has fired, cargo is
  # gone, and the check still hangs -- so the per-target cap is inert and the
  # step runs until the pod's activeDeadlineSeconds and is SIGKILLed with no
  # output at all. That surfaces as "Pod was active on the node longer than the
  # specified deadline", which reads as a slow suite rather than a hung test.
  #
  # Observed in needle-ci 2026-08-24: cargo test --lib's 900s cap fired, both
  # `timeout` and `cargo` exited, and the orphaned test binary still showed
  # fd 1 -> pipe:[49593974] while idling at 15m CPU and holding 2.6Gi. Roughly
  # 36 consecutive runs over 35h reported only the deadline message.
  #
  # Writing to a file removes the dependency on writers closing: the check
  # returns as soon as the command itself exits.
  local log exit_code=0 started_ms ended_ms duration_ms status
  log="$(mktemp "${TMPDIR:-/tmp}/dod-check-XXXXXX.log")"

  started_ms="$(needle_now_ms)"
  "$@" >"$log" 2>&1 || exit_code=$?
  ended_ms="$(needle_now_ms)"
  if (( ended_ms >= started_ms )); then
    duration_ms=$((ended_ms - started_ms))
  else
    # A wall-clock correction must not produce invalid negative telemetry.
    duration_ms=0
  fi

  # An orphan idles at ~0% CPU but holds its memory; five leaking test targets
  # would exhaust the verify container's 5Gi on their own.
  reap_orphans "$log"

  if [[ $exit_code -eq 0 ]]; then
    status="pass"
    echo "✓ $name passed"
  elif ! needle_failure_is_ours "$log"; then
    status="preexisting"
    # Someone else's in-flight file, in a checkout this commit shares.
    echo "⚠ $name failed, but every diagnostic is in a file this commit does not touch."
    echo "  Not blocking this commit. The tree is still broken — see below."
    PREEXISTING+=("$name")
    echo "Pre-existing failure details for $name (last 100 lines):"
    tail -n 100 "$log" || true
  else
    status="fail"
    echo "✗ $name failed (exit code: $exit_code)"
    FAILURES+=("$name: exit code $exit_code")
    # Show the tail here while retaining the named failure for the summary.
    # 100 lines covers cargo test's full alphabetical failures list; a 20-line
    # tail truncated the head of that list, hiding which modules failed.
    echo "Failure details for $name (last 100 lines):"
    tail -n 100 "$log" || true
  fi

  if [[ -n "${DOD_TIMING_PHASE:-}" ]]; then
    needle_duration_event "$DOD_TIMING_PHASE" "${DOD_TIMING_TARGET:-all}" \
      "$duration_ms" "$status" "$exit_code"
  fi

  rm -f "$log"
  # Keep running so every check reports its result. The summary below returns
  # the aggregate status after all requested checks have run.
  return 0
}

# Route every Cargo build and run through the same explicit slow-lane context.
# Besides making the inputs auditable, this prevents Cargo from invalidating
# the just-built test executable because the run received a different TMPDIR.
run_slow_cargo_check() {
  local phase="$1" target="$2" name="$3"
  shift 3
  DOD_TIMING_PHASE="$phase" DOD_TIMING_TARGET="$target" \
    run_check "$name" env \
      TMPDIR="$SLOW_TMPDIR" \
      CARGO_TARGET_DIR="$SLOW_CARGO_TARGET_DIR" \
      "$@"
}

# ── Attribution (--changed-only) ──────────────────────────────────────────────
#
# NEEDLE repos are worked by several agents in ONE shared checkout, so at any
# moment the tree usually holds somebody else's half-finished file. A fast lane
# that lints the whole tree therefore fails for reasons the committer did not
# cause and cannot fix without editing another worker's in-flight work — and
# the only way out is `--no-verify`. That is not hypothetical: .beads/bypasses.jsonl
# had 607 entries, 212 of them in a single day, which is a gate nobody is
# actually passing.
#
# With --changed-only the lane still runs over the whole crate (clippy and
# check are crate-scoped; there is no per-file mode), but a failure only BLOCKS
# when a diagnostic points at a path this commit stages. Anything else is
# printed in full and reported as a pre-existing tree failure, so the signal
# survives without holding the commit hostage.
STAGED_PATHS=()
if [[ "$CHANGED_ONLY" == true ]]; then
  while IFS= read -r line; do
    [[ -n "$line" ]] && STAGED_PATHS+=("$line")
  done < <(git diff --cached --name-only --diff-filter=ACMR 2>/dev/null || true)
fi

# Is $1 one of the paths this commit stages?
needle_path_is_staged() {
  local candidate="${1#./}" p
  for p in ${STAGED_PATHS[@]+"${STAGED_PATHS[@]}"}; do
    [[ "$candidate" == "$p" ]] && return 0
  done
  return 1
}

# Files named by rustc/clippy diagnostics in a --message-format short log.
# Lines look like: src/worker/mod.rs:120:9: error[E0425]: ...
needle_diagnostic_paths() {
  local log="$1"
  grep -oE '^[^ :]+\.rs:[0-9]+:[0-9]+: (error|warning)' "$log" 2>/dev/null \
    | cut -d: -f1 | sort -u
}

# Decide whether a failed check is this commit's fault.
#   0 = blame this commit (block)
#   1 = pre-existing failure elsewhere in the tree (report, do not block)
needle_failure_is_ours() {
  local log="$1" any=false path
  [[ "$CHANGED_ONLY" == true ]] || return 0
  while IFS= read -r path; do
    [[ -n "$path" ]] || continue
    any=true
    if needle_path_is_staged "$path"; then
      return 0
    fi
  done < <(needle_diagnostic_paths "$log")
  # No diagnostic could be attributed to a file at all (a link error, a
  # manifest problem, a bare "could not compile"). Stay conservative and
  # block: an unattributable failure may well be this commit's.
  [[ "$any" == true ]] || return 0
  return 1
}

# Emit a marker for the NEEDLE verification gate handler
echo "NEEDLE_VERIFICATION_GATE: definition-of-done"

# Fast lane checks (seconds, run locally)
if [[ "$LANE" == "fast" ]] || [[ "$LANE" == "all" ]]; then
  echo "=== Fast Lane Checks ==="

  # cargo fmt --check
  #
  # Formatting is the one fast-lane check that IS per-file, so --changed-only
  # scopes it properly rather than attributing after the fact: rustfmt is asked
  # about the staged .rs files and nothing else.
  if [[ "$CHANGED_ONLY" == true ]]; then
    STAGED_RS=()
    for path in ${STAGED_PATHS[@]+"${STAGED_PATHS[@]}"}; do
      [[ "$path" == *.rs && -f "$path" ]] && STAGED_RS+=("$path")
    done
    if [[ ${#STAGED_RS[@]} -gt 0 ]]; then
      run_check "rustfmt --check (staged files)" rustfmt --check --edition 2021 "${STAGED_RS[@]}"
    else
      echo "Skipping rustfmt: this commit stages no .rs files"
    fi
  else
    run_check "cargo fmt --check" cargo fmt -- --check
  fi

  # The attribution logic decides whether a failing lane blocks or is somebody
  # else's breakage, so it is itself part of the gate. Pure bash, milliseconds.
  run_check "attribution tests" bash tests/dod-attribution/run.sh

  # Same for the CI modes: --gate and --target are what keep a rejected change
  # from paying to compile a test suite. Pure bash, milliseconds.
  run_check "dod mode tests" bash tests/dod-modes/run.sh

  # cargo clippy --all-targets -- -D warnings
  #
  # This is the fast lane's only type-checking pass, and that is deliberate --
  # there is no `cargo check` after it. Clippy runs rustc's full type check
  # over a strict superset of `cargo check`'s targets (--all-targets covers
  # lib, bins, tests and benches; bare check covers lib and bins only), so a
  # second pass could never surface an error clippy missed. Nor is it a cheap
  # re-verification: the workspace's own crates keep separate fingerprints
  # between clippy and check (only the dependency graph is shared), so check
  # re-did the whole lib+bins pass even seconds after clippy had checked the
  # identical tree -- measured 2026-09-09 (needle-18a7df70) at ~18s against a
  # warm dependency cache on this box. Do not add it back.
  #
  # `--message-format short` in --changed-only mode: the one-line-per-diagnostic
  # form is what needle_failure_is_ours parses to decide whether a failure sits
  # in a file this commit stages. The default (rendered) form still shows the
  # full diagnostic, so it stays the CI/manual default.
  if [[ "$CHANGED_ONLY" == true ]]; then
    run_check "cargo clippy" cargo clippy --all-targets --message-format short -- -D warnings
  else
    run_check "cargo clippy" cargo clippy --all-targets -- -D warnings
  fi
fi

# ── Gating (--gate) ──────────────────────────────────────────────────────────
#
# A failed fast lane is a verdict: the code this run is for has already been
# rejected. Compiling every test target afterwards cannot change it, and in CI
# that is not free -- needle-ci-r97zq spent its slow lane compiling targets for
# a clippy failure decided at the five-minute mark. So --gate returns before
# the slow lane, with the same failure summary the aggregate path would print.
#
# The default stays aggregate. A human at a terminal wants every failure in one
# pass, and --gate is passed only by CI callers that have already got a verdict.
#
# Extracted and tested by tests/dod-modes/run.sh rather than left inline.
needle_gate_skips_slow_lane() {
  [[ "$GATE" == true && ${#FAILURES[@]} -gt 0 ]]
}

RUN_SLOW=false
if [[ "$LANE" == "slow" ]] || [[ "$LANE" == "all" ]]; then
  RUN_SLOW=true
fi

if needle_gate_skips_slow_lane; then
  RUN_SLOW=false
  if [[ "$LANE" == "slow" ]] || [[ "$LANE" == "all" ]]; then
    echo ""
    echo "=== Slow Lane Skipped (--gate) ==="
    echo "The fast lane already failed; no test target is compiled."
    echo "Run without --gate locally to see every failure in one pass."
  fi
fi

# Slow lane checks (tests)
if [[ "$RUN_SLOW" == true ]]; then
  echo "=== Slow Lane Checks ==="

  # Create the build-and-run context in the parent shell before either phase.
  # TMPDIR is isolated from both the checkout's .beads and a poisoned
  # /tmp/.beads. CARGO_TARGET_DIR deliberately keeps Cargo's configured cache,
  # but is resolved once so the build and every run receive the exact same
  # inputs. The trap sees the parent-shell LANE_TMPS append and cleans it.
  trap cleanup_lane_tmps EXIT
  SLOW_TMPDIR=""
  lane_tmp "${SLOW_TARGET:-all}" SLOW_TMPDIR
  SLOW_CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
  if [[ "$SLOW_CARGO_TARGET_DIR" != /* ]]; then
    SLOW_CARGO_TARGET_DIR="$REPO_ROOT/$SLOW_CARGO_TARGET_DIR"
  fi
  echo "Slow lane context: TMPDIR=$SLOW_TMPDIR CARGO_TARGET_DIR=$SLOW_CARGO_TARGET_DIR"

  # Compile every test target BEFORE the timed checks below.
  #
  # Each `timeout 900` wraps `cargo test`, which compiles AND runs. That makes
  # the cap bound compile+run rather than run, so the identical commit passes
  # with a warm sccache and fails cold. That is exactly how needle-ci went red
  # on 2026-08-25: run 6k6ff started 55s after 213f61e was pushed, built cold,
  # and was killed in verify; a later run on the same commit reported a 99.07%
  # sccache hit rate and passed with 0 failures. Seven consecutive runs failed
  # this way and read as a broken test suite.
  #
  # integration_tests alone executes ~520s against its 900s cap, so there is no
  # headroom to absorb a cold build. Building first restores each cap to what
  # it is documented to be: a bound on test execution.
  #
  # This does not add work -- it moves compilation out of windows that were
  # never meant to measure it.
  #
  # With --target, only the selected target is built: each of CI's per-target
  # pods compiles its own target's dependencies and nothing else, so five pods
  # do not each pay for five targets.
  #
  # ── Per-target runs (--target) ────────────────────────────────────────────
  #
  BUILD_LABEL="all test targets"
  BUILD_SELECTORS=()
  if [[ "$SLOW_TARGET" != "installer" ]]; then
    if [[ -n "$SLOW_TARGET" ]]; then
      BUILD_LABEL="$SLOW_TARGET"
      while IFS= read -r arg; do
        BUILD_SELECTORS+=("$arg")
      done < <(needle_cargo_selector "$SLOW_TARGET")
    else
      # No --target: build every cargo target.
      while IFS= read -r t; do
        while IFS= read -r arg; do
          BUILD_SELECTORS+=("$arg")
        done < <(needle_cargo_selector "$t")
      done < <(selected_cargo_targets)
    fi

    run_slow_cargo_check build "${SLOW_TARGET:-all}" \
      "cargo test --no-run (build $BUILD_LABEL)" \
      timeout --kill-after=30 1800 cargo test --no-run \
        ${BUILD_SELECTORS[@]+"${BUILD_SELECTORS[@]}"}
  fi

  # Standing negative control for the 2026-08-30 incident. bead-rs workspace
  # discovery stops at the first .beads it meets walking up from a temp dir,
  # so a stray /tmp/.beads on the verify pod refused every fixture's
  # `bead init` — 40 integration failures across four targets that day. The
  # fixtures no longer name /tmp (the tree-wide lint enforces that), so every
  # run now proves it: create the poison if absent and require the lanes to
  # pass with it sitting there.
  POISON_DIR="/tmp/.beads"
  POISON_WE_CREATED=0
  if [ ! -d "$POISON_DIR" ]; then
    if mkdir -p "$POISON_DIR" 2>/dev/null; then
      POISON_WE_CREATED=1
      echo "note: created negative-control $POISON_DIR for this run"
    else
      echo "warning: could not create $POISON_DIR; lanes will run without the negative control" >&2
    fi
  else
    echo "note: pre-existing $POISON_DIR found; keeping it as the negative control"
  fi

  # Core integration coverage. Each target is run from the table above, so the
  # check name stays derived from the same place the build step got its
  # selector -- the two cannot disagree. Keep each target separately named so
  # CI reports which strand phase failed, and bound every target to fit the
  # verify-step deadline while still allowing the shared debug build to
  # complete.
  #
  # The loop reads a process substitution, NOT a pipe: `run_check` appends to
  # FAILURES, and a piped `while` would do that in a subshell and lose every
  # failure it recorded.
  while IFS= read -r target; do
    SELECTOR=()
    while IFS= read -r arg; do
      SELECTOR+=("$arg")
    done < <(needle_cargo_selector "$target")
    if [[ ${#SELECTOR[@]} -eq 0 ]]; then
      echo "ERROR: no cargo selector for target '$target'" >&2
      exit 1
    fi
    run_slow_cargo_check run "$target" "cargo test ${SELECTOR[*]}" \
      timeout --kill-after=30 900 cargo test "${SELECTOR[@]}"
  done < <(selected_cargo_targets)

  # Installer tests (isolated, shell-level regression tests). Not a cargo
  # target, so a per-target run only reaches it when it is the one asked for.
  if [[ -z "$SLOW_TARGET" || "$SLOW_TARGET" == "installer" ]]; then
    DOD_TIMING_PHASE=run DOD_TIMING_TARGET=installer \
      run_check "installer tests" timeout --kill-after=30 60 bash tests/installer/run.sh
  fi
fi

# Summary report
echo ""
echo "=== Definition of Done Summary ==="
echo "Lane: $LANE"
echo "Checks run: ${#CHECKS[@]}"
echo "Failures: ${#FAILURES[@]}"

if [[ ${#PREEXISTING[@]} -gt 0 ]]; then
  echo ""
  echo "Pre-existing tree failures (NOT caused by this commit, not blocking it):"
  for name in "${PREEXISTING[@]}"; do
    echo "  - $name"
  done
  echo ""
  echo "  These are real and someone has to fix them, but every diagnostic sits"
  echo "  in a file this commit does not stage — in a checkout shared with other"
  echo "  workers that usually means an in-flight edit of theirs. Blocking here"
  echo "  would only push this commit through with --no-verify."
fi

if [[ ${#FAILURES[@]} -gt 0 ]]; then
  echo ""
  echo "Failed checks:"
  for failure in "${FAILURES[@]}"; do
    echo "  - $failure"
  done
  echo ""
  echo "❌ Definition of NOT done"
  exit 1
else
  echo "✓ Definition of Done"
  if [[ "$COUNT_BYPASS" == "true" && "${NEEDLE_PRE_COMMIT:-}" == 1 ]]; then
    if ! needle_mark_verified "$LANE"; then
      echo "ERROR: Could not record the verification result for post-commit tracking." >&2
      exit 1
    fi
  fi
  exit 0
fi
