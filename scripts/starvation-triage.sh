#!/usr/bin/env bash
#
# starvation-triage.sh — deterministic triage of NEEDLE starvation-alert beads.
#
# Why this exists
# ~~~~~~~~~~~~~~~
# Pluck used to file "Starvation alert: beads invisible in <workspace>" beads
# whose own payload read '**Open beads:** 0' while their prose claimed "open
# beads exist". Sixteen of them landed in coned-rate-optimizer alone on
# 2026-08-27, each costing a human triage pass that reached the same verdict:
# false positive, the workspace was healthy and idle. The filing bug is fixed
# upstream (pluck now files only when classification.invisible is non-empty),
# but alerts already filed by stale workers still have to be disposed of —
# mechanically, with evidence, and always through the bead CLI.
#
# What it does
# ~~~~~~~~~~~~
#   1. Detects the workspace's bead backend BEFORE running any bead command:
#        bead-rs → `bead` : .beads/config.json (+ .needle.yaml
#                            `bead_cli: backend: bead-rs`)
#        bf      → `bf`   : .beads/config.yaml
#      Anything ambiguous refuses to run. The CLIs are not interchangeable and
#      running the wrong one against a store silently corrupts its schema.
#   2. Enumerates ground truth with explicit per-status queries:
#        `list --status open --json`, `list --status in_progress --json`,
#        `list --ready --json`.
#      Bare `list --json` is never used: it returns a closed-only slice that
#      hides open beads — the exact behavior that produced the false "open
#      beads exist" claims these alerts encode.
#   3. Runs `bead doctor` for workspace health.
#   4. Decides, from ground truth (the alert bead itself excluded):
#        0 open beads      → FALSE POSITIVE → `bead close --reason` carrying the
#                            evidence: open/ready/in_progress counts, doctor
#                            summary, and the contradiction between the alert's
#                            prose and its embedded '**Open beads:**' figure.
#        open beads exist and at least one is open+unassigned+unblocked
#                          → GENUINE starvation → never closed; the per-bead
#                            list is appended to the alert's notes and normal
#                            dispatch takes it from there.
#        open beads exist but none are actionable (all assigned or blocked)
#                          → inconclusive → never closed; same note attachment.
#      The alert's own '**Open beads:**' figure is recorded as corroborating or
#      contradicting evidence but never overrides the live store. An alert whose
#      payload claims 0 while the store shows actionable beads is precisely the
#      case that must not be closed, so ground truth wins every conflict.
#   5. Mutates only through the bead CLI (`close`, `update --notes`), guarded
#      by --if-revision with a single retry on a stale revision. Nothing under
#      .beads/ is ever written by hand.
#
# Usage
# ~~~~~
#   starvation-triage.sh triage <bead-id> [--workspace DIR] [--dry-run]
#   starvation-triage.sh validate [--workspace DIR] [--skip-fixtures]
#   starvation-triage.sh classify <body-file|-> <open-excl> <ready-excl> <in-progress>
#   starvation-triage.sh backend [--workspace DIR]
#
#   triage     Full pipeline for one alert bead. Refuses beads that do not look
#              like starvation alerts (label or title prefix), no-ops on an
#              already-closed bead, and refuses to act when ground truth or
#              doctor cannot be established.
#   validate   Read-only against the live store. Runs the pure-classifier unit
#              cases (including the synthetic genuinely-invisible negative case
#              that must NOT be auto-closed), end-to-end fixtures in disposable
#              scratch bead stores, and the 16 known coned-rate-optimizer false
#              positives — classification only, never re-closing them.
#   classify   The decision core as a pure function: alert body from a file or
#              stdin, ground-truth counts as arguments. Prints the verdict.
#   backend    Print the detected backend: bead-rs | bf, or fail with a reason.
#
# Exit codes
#   0  triaged / validated / nothing to do
#   1  usage error
#   2  target bead missing or is not a starvation alert
#   3  unknown or ambiguous bead backend
#   4  inconclusive: a ground-truth query, doctor, or mutation failed — the
#      alert is left open and untouched
#   5  validation failure

set -euo pipefail

SCRIPT_NAME="$(basename "$0")"
ALERT_LABEL="starvation-alert"
ALERT_TITLE_PREFIX="Starvation alert:"
# Known false positives closed by hand on 2026-08-27 in coned-rate-optimizer.
# Kept as the classification regression set; validate never mutates them.
FIXTURES=(
  conedrat-b2aa0310 conedrat-0a70373a conedrat-42f7f637 conedrat-887b3163
  conedrat-6112a7d5 conedrat-c962dd06 conedrat-c6042559 conedrat-d51143de
  conedrat-35d564ce conedrat-8f79543e conedrat-a4dea300 conedrat-5c68e609
  conedrat-0651142a conedrat-2689302f conedrat-5b45ca11 conedrat-d5dd6aa4
)

WS_DIR=""
DRY_RUN=false
BEAD_CLI=""
SKIP_FIXTURES=false

err()  { printf '%s: %s\n' "$SCRIPT_NAME" "$*" >&2; }
info() { printf '%s: %s\n' "$SCRIPT_NAME" "$*"; }

usage() {
  awk 'NR == 1 {next} /^#/ {sub(/^# ?/, ""); print; next} {exit}' "$0"
}

require_tools() {
  command -v jq >/dev/null 2>&1 || { err "jq is required"; exit 1; }
}

# ── Backend detection ────────────────────────────────────────────────────────
#
# Prints the backend name on stdout; on ambiguity prints nothing on stdout and
# the reason on stderr, returning 1. A wrong guess here is not a recovered
# error — the two CLIs corrupt each other's schema — so ambiguity must refuse.

detect_backend() {
  local dir="$1"
  local has_rs=false has_bf=false declared=""

  [[ -f "$dir/.beads/config.json" ]] && has_rs=true
  [[ -f "$dir/.beads/config.yaml" ]] && has_bf=true
  if [[ -f "$dir/.needle.yaml" ]]; then
    declared="$(sed -n 's/^[[:space:]]*backend:[[:space:]]*//p' "$dir/.needle.yaml" | head -1 | tr -d '[:space:]')"
  fi

  if $has_rs && $has_bf; then
    err "backend ambiguous in $dir: both .beads/config.json and .beads/config.yaml present"
    return 1
  fi
  if $has_bf; then
    if [[ -n "$declared" && "$declared" != "bf" ]]; then
      err "backend conflict in $dir: .beads/config.yaml says bf but .needle.yaml declares '$declared'"
      return 1
    fi
    printf 'bf\n'
    return 0
  fi
  if $has_rs; then
    if [[ -n "$declared" && "$declared" != "bead-rs" ]]; then
      err "backend conflict in $dir: .beads/config.json says bead-rs but .needle.yaml declares '$declared'"
      return 1
    fi
    printf 'bead-rs\n'
    return 0
  fi
  err "no bead store found under $dir (.beads/config.json or .beads/config.yaml required)"
  return 1
}

select_cli() {
  local backend="$1"
  case "$backend" in
    bead-rs) BEAD_CLI="bead" ;;
    bf)      BEAD_CLI="bf" ;;
    *)       err "unknown backend '$backend'"; exit 3 ;;
  esac
  command -v "$BEAD_CLI" >/dev/null 2>&1 || {
    err "backend is $backend but the '$BEAD_CLI' CLI is not installed on this host"
    exit 3
  }
}

run_bead() {
  (cd "$WS_DIR" && "$BEAD_CLI" "$@")
}

# ── NDJSON helpers ───────────────────────────────────────────────────────────
#
# bead-rs 0.2.6 output is not one shape but three, and every list-driven
# helper must tolerate all of them:
#   * a non-empty result set reads as JSON Lines, one object per line;
#   * an EMPTY result set reads as a literal `[]` on one line — not zero
#     lines — which jq -s slurps into [[ ]], and `.[]` then iterates the
#     inner array so `select(.id …)` raises "Cannot index array with string";
#   * `list --ready --json` sometimes prefixes stdout with a human notice
#     ("Structured diagnostics written to: …"), which is not JSON at all.
# json_objects() collapses all three to a clean object stream: line-filter
# drops the notice, and per-document array iteration drains both `[]` and
# the one-element array `show --json` emits.

json_objects() { # <any bead list/show stdout> → NDJSON of bead objects
  grep '^[[:space:]]*[\[{]' | jq -c 'if type == "array" then .[]? else . end'
}

json_count_excl() { # <bead-json stdout> <exclude-id> → count of objects whose .id != exclude-id
  local json="$1" exclude="$2"
  if [[ -z "$json" ]]; then printf '0\n'; return 0; fi
  printf '%s\n' "$json" | json_objects | jq -s --arg id "$exclude" '[.[] | select(.id != $id)] | length'
}

json_field() { # <ndjson-single-object> <field> → scalar (works on arrays too)
  printf '%s\n' "$1" | jq -r --arg f "$2" '(if type == "array" then .[0] else . end)[$f] // empty'
}

json_is_starvation_alert() { # <bead-json: single object or the array `show --json` emits> → yes|no
  # `bead show --json` returns a one-element ARRAY, so unwrap it exactly as
  # json_field does. Without the unwrap, `.labels` on the array raises jq's
  # "Cannot index array with string", the substitution comes back empty, and
  # every real starvation alert is refused as "not a starvation alert" — the
  # pipeline closes nothing, ever.
  printf '%s\n' "$1" | jq -r '
    (if type == "array" then .[0] else . end) as $b |
    if (($b.labels // []) | index("'"$ALERT_LABEL"'"))
       or (($b.title // "") | startswith("'"$ALERT_TITLE_PREFIX"'"))
    then "yes" else "no" end'
}

json_ready_detail() { # <bead-json stdout> <exclude-id> → "id — title [assignee: x]" per line, sorted
  local json="$1" exclude="$2"
  [[ -z "$json" ]] && return 0
  # -s is required: without it each bead object is a separate top-level
  # document and `.[]` iterates its FIELDS, so `select(.id …)` hits the
  # array-valued fields and dies. Slurped, `.[]` iterates the beads.
  printf '%s\n' "$json" | json_objects | jq -s -r --arg id "$exclude" \
    '[.[] | select(.id != $id)] | sort_by(.id) | .[] | .id + " — " + (.title // "") + " [assignee: " + (.assignee // "none") + "]"'
}

# ── Decision core ────────────────────────────────────────────────────────────

body_open_count() { # stdin: alert body → first '**Open beads:** N' figure, or empty
  sed -n 's/^\*\*Open beads:\*\*[[:space:]]*\([0-9][0-9]*\)[[:space:]]*$/\1/p' | head -1
}

body_prose_claims_open() { # stdin: alert body → yes|no
  grep -qi 'open beads exist' && printf 'yes\n' || printf 'no\n'
}

# classify <body> <open-excl> <ready-excl> <in-progress>
# Sets CLASSIFY_VERDICT (false-positive|keep-open) and CLASSIFY_BASIS.
# Verdicts:
#   false-positive  the only safe close case: the store holds zero open beads
#                   once the alert itself is excluded.
#   keep-open       everything else — genuine starvation or inconclusive.
classify() {
  local body="$1" gt_open="$2" gt_ready="$3" gt_inprog="$4"
  local payload_count prose
  payload_count="$(printf '%s' "$body" | body_open_count)"
  prose="$(printf '%s' "$body" | body_prose_claims_open)"

  if [[ "$gt_open" == "0" ]]; then
    CLASSIFY_VERDICT="false-positive"
    if [[ -n "$payload_count" && "$payload_count" != "0" ]]; then
      CLASSIFY_BASIS="stale alert: its payload counted ${payload_count} open beads but the live store now shows 0 (excluding this alert)"
    elif [[ -z "$payload_count" ]]; then
      CLASSIFY_BASIS="ground truth: the live store shows 0 open beads (excluding this alert); the payload carries no '**Open beads:**' figure"
    else
      local prose_note=""
      if [[ "$prose" == "no" ]]; then
        prose_note=" (prose phrase absent from payload)"
      fi
      CLASSIFY_BASIS="self-contradictory: prose claims 'open beads exist'${prose_note} while its own payload reads '**Open beads:** 0', and the live store confirms 0 open beads (excluding this alert)"
    fi
  else
    CLASSIFY_VERDICT="keep-open"
    if (( gt_ready > 0 )); then
      CLASSIFY_BASIS="genuine starvation: ${gt_open} open bead(s) excluding this alert, ${gt_ready} open+unassigned+unblocked — never auto-closed"
    else
      CLASSIFY_BASIS="inconclusive: ${gt_open} open bead(s) excluding this alert but none open+unassigned+unblocked (all assigned or dependency-blocked) — not classified false-positive"
    fi
  fi
}

# ── Ground truth ─────────────────────────────────────────────────────────────
#
# Explicit per-status queries only. Bare `list --json` is a closed-only slice
# and would manufacture exactly the false "no open beads" evidence this script
# exists to correct, so it is deliberately absent. `list` caps at 100 results
# unless --limit is passed — four fleet workspaces hold 100+ open beads — so
# every query lifts the cap; a truncated result set is truncated evidence.

GROUND_TRUTH_LIMIT=999999

ground_truth() { # sets GT_OPEN_JSON / GT_INPROG_JSON / GT_READY_JSON; exits 4 on failure
  local query
  for query in "--status open" "--status in_progress" "--ready"; do
    if ! GT_JSON_TMP="$(run_bead list $query --limit "$GROUND_TRUTH_LIMIT" --json 2>/dev/null)"; then
      err "ground-truth query 'bead list $query --json' failed in $WS_DIR — refusing to act on partial evidence"
      exit 4
    fi
    case "$query" in
      "--status open")        GT_OPEN_JSON="$GT_JSON_TMP" ;;
      "--status in_progress") GT_INPROG_JSON="$GT_JSON_TMP" ;;
      "--ready")              GT_READY_JSON="$GT_JSON_TMP" ;;
    esac
  done
}

run_doctor() { # sets DOCTOR_RC / DOCTOR_SUMMARY; exits 4 if doctor fails
  local out ok=0 warn=0 fail=0 rc=0
  out="$(run_bead doctor 2>&1)" || rc=$?
  if (( rc != 0 )); then
    err "bead doctor failed (exit $rc) in $WS_DIR — workspace health unproven, refusing to act"
    exit 4
  fi
  ok="$(printf '%s\n' "$out" | grep -c '^OK' || true)"
  warn="$(printf '%s\n' "$out" | grep -c '^WARN' || true)"
  fail="$(printf '%s\n' "$out" | grep -cE '^(FAIL|ERROR)' || true)"
  DOCTOR_RC=0
  DOCTOR_SUMMARY="exit=0 (OK=${ok} WARN=${warn} FAIL=${fail})"
}

# ── triage ───────────────────────────────────────────────────────────────────

mutation_retry() { # <rc-from-first-attempt> — true when the caller may retry once
  [[ "$1" == "4" ]]
}

cmd_triage() {
  local id="" arg
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --workspace) WS_DIR="$(cd "$2" && pwd -P)"; shift 2 ;;
      --dry-run)   DRY_RUN=true; shift ;;
      -h|--help)   usage; return 0 ;;
      *) if [[ -z "$id" ]]; then id="$1"; shift; else err "unexpected argument '$1'"; return 1; fi ;;
    esac
  done
  [[ -n "$id" ]] || { err "triage requires a bead id"; return 1; }

  local backend
  backend="$(detect_backend "$WS_DIR")" || exit 3
  select_cli "$backend"
  # Label the mode accurately: DRY_RUN holds "false"/"true", and ${DRY_RUN:+…}
  # tests non-empty — "false" is non-empty, so every real run was labelled a
  # dry-run while its mutations were live. Test the value, not its emptiness.
  local dry_label=""
  if [[ "$DRY_RUN" == true ]]; then dry_label=" (dry-run)"; fi
  info "workspace=$WS_DIR backend=$backend cli=$BEAD_CLI$dry_label"

  local alert_json status revision
  if ! alert_json="$(run_bead show "$id" --json 2>/dev/null)"; then
    err "cannot read bead $id in $WS_DIR"
    exit 2
  fi
  status="$(json_field "$alert_json" status)"
  if [[ "$status" == "closed" ]]; then
    info "$id is already closed — nothing to do (idempotent no-op)"
    return 0
  fi
  if [[ "$(json_is_starvation_alert "$alert_json")" != "yes" ]]; then
    err "$id does not look like a starvation alert (needs label '$ALERT_LABEL' or title prefix '$ALERT_TITLE_PREFIX') — refusing to touch it"
    exit 2
  fi
  revision="$(json_field "$alert_json" revision)"

  ground_truth
  local gt_open gt_ready gt_inprog
  gt_open="$(json_count_excl "$GT_OPEN_JSON" "$id")"
  gt_ready="$(json_count_excl "$GT_READY_JSON" "$id")"
  gt_inprog="$(json_count_excl "$GT_INPROG_JSON" "$id")"
  run_doctor

  local body
  body="$(json_field "$alert_json" description)"
  classify "$body" "$gt_open" "$gt_ready" "$gt_inprog"
  info "ground truth (this alert excluded): open=$gt_open ready=$gt_ready in_progress=$gt_inprog; doctor $DOCTOR_SUMMARY"
  info "verdict: $CLASSIFY_VERDICT — $CLASSIFY_BASIS"

  if [[ "$CLASSIFY_VERDICT" == "false-positive" ]]; then
    local reason
    reason="[${SCRIPT_NAME} auto-triage] FALSE POSITIVE. ${CLASSIFY_BASIS}. Ground truth via explicit per-status queries (this alert excluded): open=${gt_open}, ready(open+unassigned+unblocked)=${gt_ready}, in_progress=${gt_inprog}. bead doctor: ${DOCTOR_SUMMARY}. No dispatchable work exists; the alert's prose contradicts both its payload and the live store."
    if $DRY_RUN; then
      info "[dry-run] would run: bead close $id --reason \"<evidence>\" --if-revision $revision"
      printf '  reason: %s\n' "$reason"
      return 0
    fi
    local rc=0
    run_bead close "$id" --reason "$reason" --if-revision "$revision" || rc=$?
    if mutation_retry "$rc"; then
      # Someone touched the bead between read and close. Re-read: if they
      # closed it, we are done; otherwise retry once on the fresh revision.
      if ! alert_json="$(run_bead show "$id" --json 2>/dev/null)"; then
        err "lost read on $id during retry — leaving it as found"; exit 4
      fi
      if [[ "$(json_field "$alert_json" status)" == "closed" ]]; then
        info "$id was closed concurrently — nothing to do"
        return 0
      fi
      revision="$(json_field "$alert_json" revision)"
      rc=0
      run_bead close "$id" --reason "$reason" --if-revision "$revision" || rc=$?
    fi
    if (( rc != 0 )); then
      err "bead close $id failed (exit $rc) — leaving it open"
      exit 4
    fi
    info "closed $id as false positive with evidence"
    return 0
  fi

  # keep-open: attach the per-bead evidence so normal dispatch can act.
  local note ready_list existing_notes combined
  ready_list="$(json_ready_detail "$GT_READY_JSON" "$id")"
  note="[$(date -u +%Y-%m-%dT%H:%M:%SZ) ${SCRIPT_NAME}] NOT auto-closed — ${CLASSIFY_BASIS}.
Ground truth (explicit per-status queries, this alert excluded): open=${gt_open}, ready=${gt_ready}, in_progress=${gt_inprog}. bead doctor: ${DOCTOR_SUMMARY}.
Actionable beads (open, unassigned, unblocked):
${ready_list:-  (none actionable)}"

  if $DRY_RUN; then
    info "[dry-run] would append triage evidence to the notes of $id"
    printf '%s\n' "$note"
    return 0
  fi

  existing_notes="$(json_field "$alert_json" notes)"
  combined="$note"
  if [[ -n "$existing_notes" ]]; then
    combined="$(printf '%s\n\n---\n\n%s' "$existing_notes" "$note")"
  fi
  local rc=0
  run_bead update "$id" --notes "$combined" --if-revision "$revision" || rc=$?
  if mutation_retry "$rc"; then
    if ! alert_json="$(run_bead show "$id" --json 2>/dev/null)"; then
      err "lost read on $id during retry — leaving notes untouched"; exit 4
    fi
    revision="$(json_field "$alert_json" revision)"
    existing_notes="$(json_field "$alert_json" notes)"
    combined="$note"
    if [[ -n "$existing_notes" ]]; then
      combined="$(printf '%s\n\n---\n\n%s' "$existing_notes" "$note")"
    fi
    rc=0
    run_bead update "$id" --notes "$combined" --if-revision "$revision" || rc=$?
  fi
  if (( rc != 0 )); then
    err "bead update $id failed (exit $rc) — evidence not attached; the alert remains open"
    exit 4
  fi
  info "left $id open and attached the actionable-bead evidence to its notes"
  return 0
}

# ── validate ─────────────────────────────────────────────────────────────────
#
# Read-only against the live store: the known fixtures are closed, so they are
# checked for classification only. Mutation coverage runs in disposable scratch
# bead stores, removed on success and deliberately left behind on failure.

SCRATCH_DIRS=()
cleanup_scratch() {
  if [[ "$VALIDATE_FAILED" == "true" ]]; then
    printf '%s: validation failed — leaving scratch store(s) for diagnosis: %s\n' \
      "$SCRIPT_NAME" "${SCRATCH_DIRS[*]}" >&2
    return 0
  fi
  rm -rf "${SCRATCH_DIRS[@]}"
}

make_scratch_store() { # <name> → prints dir
  local dir
  dir="$(mktemp -d "/tmp/${1}.XXXXXX")"
  SCRATCH_DIRS+=("$dir")
  (cd "$dir" && bead init >/dev/null 2>&1)
  printf '%s\n' "$dir"
}

scratch_alert_body() { # <workspace-label> <open-count-figure>
  printf 'Pluck found no candidates but open beads exist.\n\n**Workspace:** %s\n**Open beads:** %s\n**Excluded beads:** 0\n**Exclusion reasons:** \n\n**Timestamp:** %s\n' \
    "$1" "$2" "$(date -u +%Y-%m-%dT%H:%M:%S%z)"
}

# check <case-name> <expected> <actual> — records a pass/fail row
PASS=0
FAIL=0
VALIDATE_FAILED=false
check() {
  local name="$1" expected="$2" actual="$3"
  if [[ "$expected" == "$actual" ]]; then
    printf '  PASS  %s\n' "$name"
    PASS=$((PASS + 1))
  else
    printf '  FAIL  %s\n        expected: %s\n        actual:   %s\n' "$name" "$expected" "$actual"
    FAIL=$((FAIL + 1))
    VALIDATE_FAILED=true
  fi
}

unit_cases() {
  local verdict
  printf 'Unit cases (pure classifier):\n'

  # U1 — the fleet's actual false-positive shape.
  verdict="$(classify "$(scratch_alert_body scratch false-positive-branch)" 0 0 1 && printf '%s' "$CLASSIFY_VERDICT")"
  check "U1 payload '**Open beads:** 0', store 0 open → false-positive" "false-positive" "$verdict"

  # U2 — synthetic genuinely-invisible fixture: the payload claims zero while
  # two actionable beads sit invisible in the store. Must NOT auto-close.
  verdict="$(classify "$(scratch_alert_body scratch genuine-starvation)" 2 2 1 && printf '%s' "$CLASSIFY_VERDICT")"
  check "U2 payload '**Open beads:** 0', 2 open+ready beads → keep-open (negative fixture)" "keep-open" "$verdict"

  # U3 — stale payload: alert counted work that has since been dispatched.
  verdict="$(classify "$(scratch_alert_body scratch stale-payload 3)" 0 0 2 && printf '%s' "$CLASSIFY_VERDICT")"
  check "U3 payload '**Open beads:** 3', store 0 open → false-positive" "false-positive" "$verdict"

  # U4/U5 — payloads with no count figure at all decide on ground truth alone.
  verdict="$(classify "no structured payload in this body" 0 0 0 && printf '%s' "$CLASSIFY_VERDICT")"
  check "U4 no payload figure, store 0 open → false-positive" "false-positive" "$verdict"
  verdict="$(classify "no structured payload in this body" 1 1 0 && printf '%s' "$CLASSIFY_VERDICT")"
  check "U5 no payload figure, 1 open+ready bead → keep-open" "keep-open" "$verdict"

  # U6 — open beads exist but every one is assigned or blocked.
  verdict="$(classify "$(scratch_alert_body scratch all-assigned)" 3 0 0 && printf '%s' "$CLASSIFY_VERDICT")"
  check "U6 3 open but none actionable → keep-open (inconclusive, never closed)" "keep-open" "$verdict"
}

e2e_cases() {
  printf 'End-to-end cases (disposable scratch bead stores):\n'

  # E1 — false-positive branch: an alert whose store holds nothing but itself.
  local d alert_id
  d="$(make_scratch_store starvation-e2e-fp)"
  alert_id="$(cd "$d" && bead create \
    --title "${ALERT_TITLE_PREFIX} beads invisible in scratch-fp" \
    --priority 2 --issue-type task --label "$ALERT_LABEL" --label human \
    --description "$(scratch_alert_body scratch-fp 0)")"
  "$0" triage "$alert_id" --workspace "$d" >/dev/null 2>&1 || true
  check "E1 alert auto-closed with evidence" "closed" \
    "$(cd "$d" && bead show "$alert_id" --json | jq -r '.[0].status')"

  # E2 — synthetic genuinely-invisible fixture: the alert plus two real,
  # unassigned, unblocked beads. The script must leave the alert open.
  d="$(make_scratch_store starvation-e2e-real)"
  alert_id="$(cd "$d" && bead create \
    --title "${ALERT_TITLE_PREFIX} beads invisible in scratch-real" \
    --priority 2 --issue-type task --label "$ALERT_LABEL" --label human \
    --description "$(scratch_alert_body scratch-real 0)")"
  local b_id c_id plain_id rc
  b_id="$(cd "$d" && bead create --title "Real stranded work B" --priority 2)"
  c_id="$(cd "$d" && bead create --title "Real stranded work C" --priority 2)"
  plain_id="$b_id"
  "$0" triage "$alert_id" --workspace "$d" >/dev/null 2>&1 || true
  check "E2 genuinely-starved alert stays open" "open" \
    "$(cd "$d" && bead show "$alert_id" --json | jq -r '.[0].status')"
  check "E2 both stranded beads still open" "open open" \
    "$(cd "$d" && bead show "$b_id" --json | jq -r '.[0].status') $(cd "$d" && bead show "$c_id" --json | jq -r '.[0].status')"
  check "E2 evidence names the actionable beads" "yes" \
    "$(cd "$d" && bead show "$alert_id" --json | jq -r '.[0].notes' | grep -q "$c_id" && printf 'yes' || printf 'no')"

  # E3 — the identity gate: the script never closes beads that are not
  # starvation alerts, even when the store looks starved. The refusal exit
  # code is the expected outcome here, so capture it instead of letting it
  # hit set -e — a bare failing command aborts validate before the fixture
  # and summary sections ever run.
  rc=0
  "$0" triage "$plain_id" --workspace "$d" >/dev/null 2>&1 || rc=$?
  check "E3 non-alert bead refused (exit 2)" "2" "$rc"
  check "E3 refused bead untouched" "open" \
    "$(cd "$d" && bead show "$plain_id" --json | jq -r '.[0].status')"

  # E4 — backend detection on a bf-shaped store. Built directly, NOT via
  # make_scratch_store: that helper runs `bead init`, whose .beads/config.json
  # plus a config.yaml is exactly the ambiguity E5 asserts — as written E4
  # could only ever test the wrong case.
  d="$(mktemp -d "/tmp/starvation-e2e-bf.XXXXXX")"
  mkdir -p "$d/.beads" && printf 'backend: bf\n' > "$d/.beads/config.yaml"
  check "E4 config.yaml detected as bf" "bf" "$("$0" backend --workspace "$d")"
  rm -rf "$d"

  # E5 — ambiguous store refuses to run rather than guessing. Same set -e
  # consideration as E3: the refusal exit code is the expected outcome.
  d="$(make_scratch_store starvation-e2e-ambiguous)"
  mkdir -p "$d/.beads" && printf 'backend: bf\n' > "$d/.beads/config.yaml"
  rc=0
  "$0" backend --workspace "$d" >/dev/null 2>&1 || rc=$?
  check "E5 ambiguous backend refused (exit 3)" "3" "$rc"
  rm -rf "$d"; SCRATCH_DIRS=("${SCRATCH_DIRS[@]/$d/}")
}

fixture_cases() {
  printf 'Known false positives (closed; classification only, never re-closed):\n'
  local id alert_json status title payload_count verdict
  for id in "${FIXTURES[@]}"; do
    if ! alert_json="$(run_bead show "$id" --json 2>/dev/null)"; then
      check "fixture $id readable in $WS_DIR" "present" "missing"
      continue
    fi
    status="$(json_field "$alert_json" status)"
    title="$(json_field "$alert_json" title)"
    if [[ "$status" != "closed" ]]; then
      # A fixture that somehow reopened is reported, not mutated: validate
      # never closes anything in the live store.
      printf '  WARN  fixture %s is %s, expected closed — left untouched\n' "$id" "$status"
      continue
    fi
    check "fixture $id is a starvation alert" "yes" "$(json_is_starvation_alert "$alert_json")"
    payload_count="$(json_field "$alert_json" description | body_open_count)"
    check "fixture $id payload reads '**Open beads:** 0'" "0" "$payload_count"
    verdict="$(classify "$(json_field "$alert_json" description)" 0 0 1 && printf '%s' "$CLASSIFY_VERDICT")"
    check "fixture $id classifies false-positive" "false-positive" "$verdict"
    printf '        (title: %s)\n' "$title"
  done
}

cmd_validate() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --workspace)     WS_DIR="$(cd "$2" && pwd -P)"; shift 2 ;;
      --skip-fixtures) SKIP_FIXTURES=true; shift ;;
      -h|--help)       usage; return 0 ;;
      *) err "unexpected argument '$1'"; return 1 ;;
    esac
  done

  local backend
  backend="$(detect_backend "$WS_DIR")" || exit 3
  select_cli "$backend"
  info "validating against $WS_DIR (backend=$backend) — read-only for the live store"

  unit_cases
  e2e_cases
  if [[ "$SKIP_FIXTURES" != "true" ]]; then
    fixture_cases
  fi

  printf 'Summary: %d passed, %d failed%s\n' "$PASS" "$FAIL" \
    "$([[ "$SKIP_FIXTURES" == "true" ]] && printf ' (fixtures skipped)' || printf '')"
  if [[ "$FAIL" -gt 0 ]]; then
    exit 5
  fi
  printf 'validate performed zero mutations in %s\n' "$WS_DIR"
  return 0
}

# ── classify / backend entry points ──────────────────────────────────────────

cmd_classify() {
  local body_file="$1" gt_open="${2:-}" gt_ready="${3:-}" gt_inprog="${4:-}"
  [[ -n "$gt_open" && -n "$gt_ready" && -n "$gt_inprog" ]] || {
    err "classify requires: <body-file|-> <open-excl> <ready-excl> <in-progress>"; return 1
  }
  for v in "$gt_open" "$gt_ready" "$gt_inprog"; do
    [[ "$v" =~ ^[0-9]+$ ]] || { err "ground-truth counts must be non-negative integers, got '$v'"; return 1; }
  done
  local body
  body="$(if [[ "$body_file" == "-" ]]; then cat; else cat "$body_file"; fi)"
  classify "$body" "$gt_open" "$gt_ready" "$gt_inprog"
  printf '%s\n' "$CLASSIFY_VERDICT"
  printf 'basis: %s\n' "$CLASSIFY_BASIS" >&2
}

cmd_backend() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --workspace) WS_DIR="$(cd "$2" && pwd -P)"; shift 2 ;;
      -h|--help)   usage; return 0 ;;
      *) err "unexpected argument '$1'"; return 1 ;;
    esac
  done
  local backend
  backend="$(detect_backend "$WS_DIR")" || exit 3
  printf '%s\n' "$backend"
}

# ── main ─────────────────────────────────────────────────────────────────────

main() {
  require_tools
  WS_DIR="$(pwd -P)"
  [[ $# -ge 1 ]] || { usage; exit 1; }
  local subcommand="$1"
  shift

  case "$subcommand" in
    triage)    cmd_triage "$@" ;;
    validate)  SCRATCH_DIRS=(); trap cleanup_scratch EXIT; cmd_validate "$@" ;;
    classify)  cmd_classify "$@" ;;
    backend)   cmd_backend "$@" ;;
    -h|--help) usage; exit 0 ;;
    *)         err "unknown subcommand '$subcommand'"; usage; exit 1 ;;
  esac
}

main "$@"
