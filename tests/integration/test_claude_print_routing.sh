#!/usr/bin/env bash
# Live, host-specific end-to-end test for model-based adapter routing.
#
# This is intentionally not part of automated test suites. It consumes real
# agent capacity and briefly renames the host's claude-print executable for the
# negative scenario. Run it only on the NEEDLE host with the explicit opt-in:
#
#   NEEDLE_ROUTING_E2E_ACK=I_UNDERSTAND \
#     tests/integration/test_claude_print_routing.sh

set -euo pipefail

readonly REQUIRED_ACK="I_UNDERSTAND"
readonly ORIGINAL_PATH="$PATH"
readonly HOST_HOME="${NEEDLE_ROUTING_HOST_HOME:-$HOME}"
readonly HOST_XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$HOST_HOME/.config}"
readonly NEEDLE_BIN="${NEEDLE_BIN:-$HOST_HOME/.needle/bin/needle-stable}"
readonly ADAPTERS_DIR="${NEEDLE_ADAPTERS_DIR:-$HOST_HOME/.config/needle/adapters}"
readonly SCRATCH_ROOT="${NEEDLE_ROUTING_TMPDIR:-$HOST_HOME/scratch}"
readonly SELECTED_SCENARIOS="${NEEDLE_ROUTING_E2E_SCENARIOS:-all}"

TEST_ROOT=""
TEST_HOME=""
LOG_DIR=""
WRAPPER_DIR=""
CLAUDE_PRINT_BIN=""
CLAUDE_PRINT_BACKUP=""
WORKER_PID=""

log() {
    printf '[routing-e2e] %s\n' "$*"
}

pass() {
    printf '[routing-e2e] PASS: %s\n' "$*"
}

die() {
    printf '[routing-e2e] FAIL: %s\n' "$*" >&2
    exit 1
}

restore_claude_print() {
    if [[ -n "$CLAUDE_PRINT_BACKUP" && -e "$CLAUDE_PRINT_BACKUP" ]]; then
        if [[ -e "$CLAUDE_PRINT_BIN" ]]; then
            printf '[routing-e2e] refusing to overwrite restored claude-print at %s\n' \
                "$CLAUDE_PRINT_BIN" >&2
            return 1
        fi
        mv -- "$CLAUDE_PRINT_BACKUP" "$CLAUDE_PRINT_BIN"
        CLAUDE_PRINT_BACKUP=""
        log "restored $CLAUDE_PRINT_BIN"
    fi
}

cleanup() {
    local exit_code=$?

    if [[ -n "$WORKER_PID" ]] && kill -0 "$WORKER_PID" 2>/dev/null; then
        kill -TERM -- "-$WORKER_PID" 2>/dev/null || true
    fi
    restore_claude_print || exit_code=1

    if [[ $exit_code -eq 0 && -n "$TEST_ROOT" && \
          "$TEST_ROOT" == "$SCRATCH_ROOT"/needle-routing-e2e.* ]]; then
        rm -rf -- "$TEST_ROOT"
    elif [[ -n "$TEST_ROOT" ]]; then
        printf '[routing-e2e] retained failure artifacts at %s\n' "$TEST_ROOT" >&2
    fi

    exit "$exit_code"
}
trap cleanup EXIT INT TERM

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

scenario_enabled() {
    local scenario=$1
    [[ ",$SELECTED_SCENARIOS," == *,all,* || ",$SELECTED_SCENARIOS," == *,"$scenario",* ]]
}

validate_scenarios() {
    local scenarios=()
    local scenario

    IFS=',' read -r -a scenarios <<<"$SELECTED_SCENARIOS"
    ((${#scenarios[@]} > 0)) || die "no routing scenarios selected"
    for scenario in "${scenarios[@]}"; do
        case "$scenario" in
            all | sonnet | glm47 | missing) ;;
            *) die "unknown routing scenario: $scenario" ;;
        esac
    done
}

write_claude_print_probe() {
    # The probe records only non-sensitive routing evidence, never the whole
    # command line or environment. It then execs the real host binary.
    cat >"$WRAPPER_DIR/claude-print" <<'PROBE'
#!/usr/bin/env bash
set -euo pipefail

original_args=("$@")
model=""
output_format=""
while (($#)); do
    case "$1" in
        --model|-m)
            model="${2:-}"
            shift 2
            ;;
        --model=*)
            model="${1#*=}"
            shift
            ;;
        --output-format|-o)
            output_format="${2:-}"
            shift 2
            ;;
        --output-format=*)
            output_format="${1#*=}"
            shift
            ;;
        *)
            shift
            ;;
    esac
done

printf 'claude-print\t%s\t%s\n' "$model" "$output_format" \
    >>"$NEEDLE_E2E_INVOKE_LOG"
exec "$NEEDLE_E2E_REAL_CLAUDE_PRINT" "${original_args[@]}"
PROBE
    chmod 0755 "$WRAPPER_DIR/claude-print"
}

write_workspace_config() {
    local workspace=$1

    cat >"$workspace/.needle.yaml" <<EOF
agent:
  default: claude-sonnet
  args: []
  timeout: 600
  adapters_dir: $ADAPTERS_DIR
  routing:
    rules:
      - match_model: (claude-)?(sonnet|opus|fable|haiku).*
        adapter: claude-print
    default_adapter: claude-code-glm-4.7
    strict: false
bead_cli:
  backend: bead-rs
  path: $(command -v bead)
worker:
  max_workers: 1
  launch_stagger_seconds: 0
  idle_timeout: 1
  idle_action: exit
  allow_exit_without_supervisor: true
  max_claim_retries: 1
  enforce_shipped_work: false
  freshness_check_interval_secs: 0
workspace:
  default: $workspace
  home: $TEST_HOME/.needle
  labels: []
strands:
  explore:
    enabled: false
    workspaces:
      - $workspace
    workspace_root: $workspace
  mitosis:
    enabled: false
  weave:
    enabled: false
  unravel:
    enabled: false
  pulse:
    enabled: false
  reflect:
    enabled: false
  splice:
    enabled: false
telemetry:
  file_sink:
    enabled: true
    log_dir: $LOG_DIR
    retention_days: 1
  stdout_sink:
    enabled: false
  otlp_sink:
    enabled: false
gates: []
self_modification:
  enabled: false
EOF
    # Several strand toggles and adapters_dir are process-level rather than
    # workspace-overridable. Use the same isolated config as the global layer
    # so failures cannot trigger Mitosis or discover unrelated workspaces.
    cp "$workspace/.needle.yaml" "$TEST_HOME/.config/needle/config.yaml"
}

init_workspace() {
    local scenario=$1
    local workspace="$TEST_ROOT/$scenario"
    local remote="$TEST_ROOT/remotes/$scenario.git"

    mkdir -p "$workspace" "$TEST_ROOT/remotes"
    write_workspace_config "$workspace"
    printf '# NEEDLE routing E2E fixture\n' >"$workspace/README.md"
    printf '.beads/\n.needle-predispatch-sha\n' >"$workspace/.gitignore"

    git init --bare --quiet "$remote"
    git -C "$workspace" init --quiet --initial-branch=main
    git -C "$workspace" config user.name needle-routing-e2e
    git -C "$workspace" config user.email needle-routing-e2e@invalid
    git -C "$workspace" add README.md .gitignore .needle.yaml
    git -C "$workspace" commit --quiet -m "test fixture baseline"
    git -C "$workspace" remote add origin "$remote"
    git -C "$workspace" push --quiet --set-upstream origin main

    (cd "$workspace" && bead init --prefix route >/dev/null)
    printf '%s\n' "$workspace"
}

create_probe_bead() {
    local workspace=$1
    local scenario=$2
    local model=$3

    (cd "$workspace" && bead create \
        --title "Routing E2E probe: $scenario" \
        --priority 0 \
        --issue-type test \
        --description "Model request: $model. Create route-$scenario.txt containing exactly ROUTING_E2E_OK, stage only that file, and commit it. Do not change existing files or manage the bead lifecycle; NEEDLE owns the lifecycle.")
}

run_worker() {
    local workspace=$1
    local requested_adapter=$2
    local identifier=$3
    local bead_id=$4
    local invoke_log=$5
    local output_log=$6
    local worker_name="$requested_adapter-$identifier"
    local telemetry=""
    local exit_code=0

    (
        cd "$workspace"
        exec env \
            HOME="$TEST_HOME" \
            XDG_CONFIG_HOME="$TEST_HOME/.config" \
            CLAUDE_CONFIG_DIR="$HOST_HOME/.claude" \
            PATH="$WRAPPER_DIR:$ORIGINAL_PATH" \
            NEEDLE_INNER=1 \
            NEEDLE_E2E_INVOKE_LOG="$invoke_log" \
            NEEDLE_E2E_REAL_CLAUDE_PRINT="$CLAUDE_PRINT_BIN" \
            setsid timeout --signal=TERM --kill-after=10s 900s \
            "$NEEDLE_BIN" run \
                --workspace "$workspace" \
                --agent "$requested_adapter" \
                --identifier "$identifier" \
                --hot-reload false
    ) >"$output_log" 2>&1 &
    WORKER_PID=$!

    for _ in $(seq 1 3600); do
        if ! kill -0 "$WORKER_PID" 2>/dev/null; then
            wait "$WORKER_PID" || exit_code=$?
            WORKER_PID=""
            return "$exit_code"
        fi

        telemetry=$(telemetry_file "$worker_name")
        if [[ -n "$telemetry" ]] && jq -e --arg bead "$bead_id" \
            'select(.event_type == "agent.completed" and .bead_id == $bead and
                    .data.exit_code != 0)' "$telemetry" >/dev/null; then
            kill -TERM -- "-$WORKER_PID" 2>/dev/null || true
            wait "$WORKER_PID" 2>/dev/null || true
            WORKER_PID=""
            return 1
        fi
        sleep 0.25
    done

    kill -TERM -- "-$WORKER_PID" 2>/dev/null || true
    wait "$WORKER_PID" 2>/dev/null || true
    WORKER_PID=""
    return 124
}

telemetry_file() {
    local worker_name=$1
    find "$LOG_DIR" -maxdepth 1 -type f \
        -name "$worker_name-????????-*.jsonl" ! -name '*.agent.jsonl' \
        -print -quit
}

bead_status() {
    local workspace=$1
    local bead_id=$2
    (cd "$workspace" && bead list --json --limit 1000) |
        jq -r --arg id "$bead_id" 'select(.id == $id) | .status'
}

assert_routing_event() {
    local telemetry=$1
    local bead_id=$2
    local model=$3
    local chosen_adapter=$4

    jq -e \
        --arg bead "$bead_id" \
        --arg model "$model" \
        --arg adapter "$chosen_adapter" \
        'select(
            .event_type == "agent.routing_decision" and
            .bead_id == $bead and
            .data.model == $model and
            .data.chosen_adapter == $adapter
        )' "$telemetry" >/dev/null ||
        die "missing routing event for $bead_id -> $chosen_adapter"
}

assert_agent_event() {
    local telemetry=$1
    local bead_id=$2
    local chosen_adapter=$3
    local expected_exit=$4

    jq -e \
        --arg bead "$bead_id" \
        --arg adapter "$chosen_adapter" \
        --argjson exit_code "$expected_exit" \
        'select(
            .event_type == "agent.completed" and
            .bead_id == $bead and
            .data.agent == $adapter and
            .data.exit_code == $exit_code
        )' "$telemetry" >/dev/null ||
        die "missing agent.completed for $bead_id ($chosen_adapter, exit $expected_exit)"
}

assert_jsonl_stream() {
    local agent_log=$1

    [[ -s "$agent_log" ]] || die "normalized agent stream is empty: $agent_log"
    jq -e -s \
        'length > 0 and all(.[]; type == "object" and (.type | type == "string"))' \
        "$agent_log" >/dev/null ||
        die "normalized agent output is not valid stream JSON: $agent_log"
}

verify_success() {
    local scenario=$1
    local workspace=$2
    local bead_id=$3
    local requested_adapter=$4
    local identifier=$5
    local model=$6
    local chosen_adapter=$7

    local worker_name="$requested_adapter-$identifier"
    local telemetry
    telemetry=$(telemetry_file "$worker_name")
    [[ -n "$telemetry" ]] || die "telemetry file not found for $worker_name"

    assert_routing_event "$telemetry" "$bead_id" "$model" "$chosen_adapter"
    assert_agent_event "$telemetry" "$bead_id" "$chosen_adapter" 0
    jq -e --arg bead "$bead_id" \
        'select(.event_type == "bead.completed" and .bead_id == $bead)' \
        "$telemetry" >/dev/null || die "missing bead.completed for $bead_id"

    [[ "$(bead_status "$workspace" "$bead_id")" == "closed" ]] ||
        die "bead did not close: $bead_id"
    grep -Fxq 'ROUTING_E2E_OK' "$workspace/route-$scenario.txt" ||
        die "probe artifact is missing or incorrect for $bead_id"
    git -C "$workspace" log -1 --format=%H -- "route-$scenario.txt" | grep -Eq '^[0-9a-f]{40}$' ||
        die "probe artifact was not committed for $bead_id"

    jq -e \
        --arg adapter "$chosen_adapter" \
        --arg model "$model" \
        '.agent == $adapter and .model == $model and .exit_code == 0' \
        "$workspace/.beads/traces/$bead_id/metadata.json" >/dev/null ||
        die "trace metadata does not identify the successful routed adapter"

    assert_jsonl_stream "$LOG_DIR/$worker_name-$bead_id.agent.jsonl"
    assert_jsonl_stream "$workspace/.beads/traces/$bead_id/trace.jsonl"

    pass "$scenario bead $bead_id completed through $chosen_adapter"
    jq -c --arg bead "$bead_id" \
        'select(.event_type == "agent.routing_decision" and .bead_id == $bead) |
         {event_type, bead_id, data}' "$telemetry" | head -1
}

run_success_scenario() {
    local scenario=$1
    local requested_adapter=$2
    local model=$3
    local chosen_adapter=$4
    local identifier="e2e-$scenario-$$"
    local workspace
    local bead_id
    local invoke_log="$TEST_ROOT/$scenario.invoke.tsv"
    local output_log="$TEST_ROOT/$scenario.worker.log"

    workspace=$(init_workspace "$scenario")
    bead_id=$(create_probe_bead "$workspace" "$scenario" "$model")
    log "dispatching $bead_id with requested adapter $requested_adapter"
    run_worker "$workspace" "$requested_adapter" "$identifier" "$bead_id" \
        "$invoke_log" "$output_log" ||
        die "worker failed in $scenario; see $output_log"

    verify_success "$scenario" "$workspace" "$bead_id" "$requested_adapter" \
        "$identifier" "$model" "$chosen_adapter"

    if [[ "$chosen_adapter" == "claude-print" ]]; then
        awk -F '\t' '$1 == "claude-print" && $2 == "claude-sonnet-4-6" {found=1} END {exit !found}' \
            "$invoke_log" || die "claude-print invocation probe did not fire"
        pass "invoke command executed claude-print for claude-sonnet-4-6"
    elif [[ -e "$invoke_log" ]]; then
        die "negative control unexpectedly invoked claude-print"
    fi
}

run_missing_binary_scenario() {
    local scenario="missing"
    local requested_adapter="claude-sonnet"
    local model="claude-sonnet-4-6"
    local identifier="e2e-$scenario-$$"
    local workspace
    local bead_id
    local invoke_log="$TEST_ROOT/$scenario.invoke.tsv"
    local output_log="$TEST_ROOT/$scenario.worker.log"
    local trace_stderr
    local telemetry
    local worker_name="$requested_adapter-$identifier"
    local failure_seen=0

    workspace=$(init_workspace "$scenario")
    bead_id=$(create_probe_bead "$workspace" "$scenario" "$model")
    trace_stderr="$workspace/.beads/traces/$bead_id/stderr.txt"

    if pgrep -x claude-print >/dev/null 2>&1; then
        die "another claude-print process is active; refusing the global rename scenario"
    fi

    CLAUDE_PRINT_BACKUP="$CLAUDE_PRINT_BIN.needle-routing-e2e.$$"
    [[ ! -e "$CLAUDE_PRINT_BACKUP" ]] || die "backup path already exists: $CLAUDE_PRINT_BACKUP"
    mv -- "$CLAUDE_PRINT_BIN" "$CLAUDE_PRINT_BACKUP"
    log "temporarily renamed claude-print for $bead_id"

    (
        cd "$workspace"
        exec env \
            HOME="$TEST_HOME" \
            XDG_CONFIG_HOME="$TEST_HOME/.config" \
            CLAUDE_CONFIG_DIR="$HOST_HOME/.claude" \
            PATH="$WRAPPER_DIR:$ORIGINAL_PATH" \
            NEEDLE_INNER=1 \
            NEEDLE_E2E_INVOKE_LOG="$invoke_log" \
            NEEDLE_E2E_REAL_CLAUDE_PRINT="$CLAUDE_PRINT_BIN" \
            setsid timeout --signal=TERM --kill-after=10s 90s \
            "$NEEDLE_BIN" run \
                --workspace "$workspace" \
                --agent "$requested_adapter" \
                --identifier "$identifier" \
                --hot-reload false
    ) >"$output_log" 2>&1 &
    WORKER_PID=$!

    for _ in $(seq 1 240); do
        if [[ -s "$trace_stderr" ]] &&
           grep -Eqi 'claude-print.*(No such file|not found)' "$trace_stderr"; then
            failure_seen=1
            break
        fi
        if ! kill -0 "$WORKER_PID" 2>/dev/null; then
            break
        fi
        sleep 0.25
    done

    if kill -0 "$WORKER_PID" 2>/dev/null; then
        kill -TERM -- "-$WORKER_PID" 2>/dev/null || true
        sleep 0.2
    fi
    restore_claude_print
    wait "$WORKER_PID" 2>/dev/null || true
    WORKER_PID=""

    [[ $failure_seen -eq 1 ]] || die "missing claude-print did not produce a loud trace failure"
    [[ -x "$CLAUDE_PRINT_BIN" ]] || die "claude-print was not restored"

    telemetry=$(telemetry_file "$worker_name")
    [[ -n "$telemetry" ]] || die "telemetry file not found for missing-binary scenario"
    assert_routing_event "$telemetry" "$bead_id" "$model" "claude-print"
    assert_agent_event "$telemetry" "$bead_id" "claude-print" 127
    if jq -e --arg bead "$bead_id" \
        'select(.bead_id == $bead and .event_type == "agent.completed" and
                .data.agent == "claude-sonnet")' "$telemetry" >/dev/null; then
        die "missing binary silently fell back to claude-sonnet"
    fi
    [[ "$(bead_status "$workspace" "$bead_id")" != "closed" ]] ||
        die "missing-binary bead unexpectedly closed"
    awk -F '\t' '$1 == "claude-print" {found=1} END {exit !found}' "$invoke_log" ||
        die "routed claude-print invocation was not attempted"

    pass "missing claude-print failed loudly with exit 127 and no API fallback"
}

main() {
    [[ "${NEEDLE_ROUTING_E2E_ACK:-}" == "$REQUIRED_ACK" ]] ||
        die "set NEEDLE_ROUTING_E2E_ACK=$REQUIRED_ACK to run this live host test"
    validate_scenarios

    for command_name in bead git jq timeout setsid pgrep awk; do
        require_command "$command_name"
    done
    [[ -x "$NEEDLE_BIN" ]] || die "NEEDLE binary is not executable: $NEEDLE_BIN"
    [[ -d "$ADAPTERS_DIR" ]] || die "adapter directory not found: $ADAPTERS_DIR"
    [[ -f "$ADAPTERS_DIR/claude-print.yaml" ]] || die "claude-print adapter YAML missing"
    [[ -f "$ADAPTERS_DIR/claude-code-glm-4.7.yaml" ]] || die "GLM adapter YAML missing"

    CLAUDE_PRINT_BIN=$(command -v claude-print) || die "claude-print is not on PATH"
    [[ -x "$CLAUDE_PRINT_BIN" ]] || die "claude-print is not executable: $CLAUDE_PRINT_BIN"

    mkdir -p "$SCRATCH_ROOT"
    TEST_ROOT=$(mktemp -d "$SCRATCH_ROOT/needle-routing-e2e.XXXXXX")
    TEST_HOME="$TEST_ROOT/home"
    LOG_DIR="$TEST_HOME/.needle/logs"
    WRAPPER_DIR="$TEST_ROOT/bin"
    mkdir -p "$LOG_DIR" "$WRAPPER_DIR" "$TEST_HOME/.config/needle"
    # adapters_dir is process-level in the installed 0.5.0 config merge, so a
    # workspace override cannot move it out of the isolated HOME. Expose only
    # the host adapter definitions at the default isolated location.
    ln -s "$ADAPTERS_DIR" "$TEST_HOME/.config/needle/adapters"
    if [[ -d "$HOST_XDG_CONFIG_HOME/claude-print" ]]; then
        ln -s "$HOST_XDG_CONFIG_HOME/claude-print" "$TEST_HOME/.config/claude-print"
    fi
    write_claude_print_probe

    log "NEEDLE: $($NEEDLE_BIN version | head -1)"
    log "isolated root: $TEST_ROOT"

    if scenario_enabled sonnet; then
        # Scenarios 1 and 3: subscription routing, normalized stream JSON,
        # closure, and the real routing-decision event.
        run_success_scenario "sonnet" "claude-sonnet" "claude-sonnet-4-6" \
            "claude-print"
    fi

    if scenario_enabled glm47; then
        # Scenarios 2 and 3: GLM negative control and its routing-decision event.
        run_success_scenario "glm47" "claude-code-glm-4.7" "glm-4.7" \
            "claude-code-glm-4.7"
    fi

    if scenario_enabled missing; then
        # Scenario 4: global binary rename, loud failure, immediate restoration.
        run_missing_binary_scenario
    fi

    pass "selected claude-print routing scenarios: $SELECTED_SCENARIOS"
}

main "$@"
