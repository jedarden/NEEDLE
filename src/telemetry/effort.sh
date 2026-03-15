#!/usr/bin/env bash
# NEEDLE Effort/Cost Tracking Module
# Calculate and track agent costs per bead for budgeting
#
# This module handles:
# - Token cost calculation based on agent pricing models
# - Daily spend tracking and cumulative totals
# - Effort telemetry event emission
#
# Pricing models supported:
# - pay_per_token: Standard per-token pricing (input_per_1k, output_per_1k)
# - unlimited: Flat rate or free (cost = 0)
# - use_or_lose: Prepaid allocation (tracks usage but no incremental cost)

# Source dependencies if not already loaded
if [[ -z "${_NEEDLE_OUTPUT_LOADED:-}" ]]; then
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
fi

# Module version
_NEEDLE_EFFORT_VERSION="1.0.0"

# Default daily spend file
NEEDLE_DAILY_SPEND_FILE="${NEEDLE_DAILY_SPEND_FILE:-}"

# -----------------------------------------------------------------------------
# Initialization
# -----------------------------------------------------------------------------

# Initialize the effort tracking module
# Sets up the daily spend file path
_needle_effort_init() {
    if [[ -z "$NEEDLE_DAILY_SPEND_FILE" ]]; then
        NEEDLE_DAILY_SPEND_FILE="$NEEDLE_HOME/$NEEDLE_STATE_DIR/daily_spend.json"
    fi

    # Ensure state directory exists
    local state_dir
    state_dir=$(dirname "$NEEDLE_DAILY_SPEND_FILE")
    if [[ ! -d "$state_dir" ]]; then
        mkdir -p "$state_dir" || {
            _needle_error "Failed to create state directory: $state_dir"
            return 1
        }
    fi

    # Initialize daily spend file if it doesn't exist
    if [[ ! -f "$NEEDLE_DAILY_SPEND_FILE" ]]; then
        echo '{}' > "$NEEDLE_DAILY_SPEND_FILE"
    fi
}

# -----------------------------------------------------------------------------
# Cost Configuration Loading
# -----------------------------------------------------------------------------

# Get cost configuration for an agent
# Usage: _needle_get_agent_cost_config <agent_name>
# Returns: JSON object with type, input_per_1k, output_per_1k
_needle_get_agent_cost_config() {
    local agent_name="$1"

    # Default cost config (free/unlimited)
    local default_config='{"type":"unlimited","input_per_1k":0,"output_per_1k":0}'

    # Try to load agent config if loader is available
    if [[ -z "${NEEDLE_AGENT[name]:-}" ]] || [[ "${NEEDLE_AGENT[name]:-}" != "$agent_name" ]]; then
        # Source loader if needed
        if ! declare -p NEEDLE_AGENT &>/dev/null; then
            local loader_path
            loader_path="$(dirname "${BASH_SOURCE[0]}")/../agent/loader.sh"
            if [[ -f "$loader_path" ]]; then
                source "$loader_path"
            fi
        fi

        # Try to load the agent
        if declare -f _needle_load_agent &>/dev/null; then
            _needle_load_agent "$agent_name" 2>/dev/null || true
        fi
    fi

    # Check for cost configuration in agent config file
    local agent_file="${NEEDLE_AGENT[_file]:-}"
    if [[ -z "$agent_file" ]]; then
        # Try to find agent config directly
        if declare -f _needle_find_agent_config &>/dev/null; then
            agent_file=$(_needle_find_agent_config "$agent_name" 2>/dev/null)
        fi
    fi

    if [[ -n "$agent_file" && -f "$agent_file" ]]; then
        # Parse cost section using yq or Python
        local cost_config
        if command -v yq &>/dev/null; then
            cost_config=$(yq '.cost' "$agent_file" 2>/dev/null)
            if [[ -n "$cost_config" && "$cost_config" != "null" ]]; then
                echo "$cost_config"
                return 0
            fi
        elif command -v python3 &>/dev/null; then
            cost_config=$(python3 -c "
import yaml
import json
import sys

try:
    with open('$agent_file', 'r') as f:
        data = yaml.safe_load(f)

    cost = data.get('cost', {})
    if not cost:
        print('$default_config')
    else:
        # Ensure all fields exist with defaults
        cost.setdefault('type', 'unlimited')
        cost.setdefault('input_per_1k', 0)
        cost.setdefault('output_per_1k', 0)
        print(json.dumps(cost))
except Exception as e:
    print('$default_config', file=sys.stderr)
    sys.exit(1)
" 2>/dev/null)
            if [[ $? -eq 0 && -n "$cost_config" ]]; then
                echo "$cost_config"
                return 0
            fi
        fi
    fi

    # Return default config
    echo "$default_config"
}

# Get specific cost field from agent config
# Usage: _needle_get_cost_field <agent_name> <field>
# field: type, input_per_1k, output_per_1k
_needle_get_cost_field() {
    local agent_name="$1"
    local field="$2"

    local config
    config=$(_needle_get_agent_cost_config "$agent_name")

    if command -v jq &>/dev/null; then
        jq -r ".$field // 0" <<< "$config" 2>/dev/null
    else
        # Fallback: parse manually
        python3 -c "
import json
import sys

try:
    data = json.loads('''$config''')
    value = data.get('$field', 0)
    if value is None:
        value = 0
    print(value)
except:
    print(0)
" 2>/dev/null
    fi
}

# -----------------------------------------------------------------------------
# Cost Calculation
# -----------------------------------------------------------------------------

# Calculate cost for token usage
# Usage: calculate_cost <agent_name> <input_tokens> <output_tokens>
# Returns: Cost in dollars (floating point, e.g., "0.001234")
#
# Pricing models:
# - pay_per_token: (input_tokens/1000 * input_rate) + (output_tokens/1000 * output_rate)
# - unlimited: 0.00 (no per-use cost)
# - use_or_lose: 0.00 (prepaid, but usage is tracked)
#
# Example:
#   cost=$(calculate_cost "claude-anthropic-sonnet" 10000 5000)
#   echo "Cost: \$$cost"
calculate_cost() {
    local agent="${1:-unknown}"
    local input_tokens="${2:-0}"
    local output_tokens="${3:-0}"

    # Ensure tokens are numeric
    [[ ! "$input_tokens" =~ ^[0-9]+$ ]] && input_tokens=0
    [[ ! "$output_tokens" =~ ^[0-9]+$ ]] && output_tokens=0

    # Get cost configuration
    local cost_type input_rate output_rate
    cost_type=$(_needle_get_cost_field "$agent" "type")

    # Handle unlimited and use_or_lose models (no incremental cost)
    case "$cost_type" in
        unlimited|use_or_lose)
            echo "0.00"
            return 0
            ;;
    esac

    # For pay_per_token, calculate actual cost
    input_rate=$(_needle_get_cost_field "$agent" "input_per_1k")
    output_rate=$(_needle_get_cost_field "$agent" "output_per_1k")

    # Default to 0 if rates are empty or null
    [[ -z "$input_rate" || "$input_rate" == "null" ]] && input_rate=0
    [[ -z "$output_rate" || "$output_rate" == "null" ]] && output_rate=0

    # Calculate cost: (tokens / 1000) * rate_per_1k
    local cost
    if command -v bc &>/dev/null; then
        cost=$(echo "scale=6; ($input_tokens/1000)*$input_rate + ($output_tokens/1000)*$output_rate" | bc 2>/dev/null)
    elif command -v awk &>/dev/null; then
        cost=$(awk "BEGIN {printf \"%.6f\", ($input_tokens/1000)*$input_rate + ($output_tokens/1000)*$output_rate}" 2>/dev/null)
    else
        # Fallback: basic shell arithmetic (integer only, less precise)
        cost=$(( (input_tokens * input_rate + output_tokens * output_rate) / 1000 ))
        # Convert to decimal format
        cost="0.$cost"
    fi

    # Handle empty result
    if [[ -z "$cost" ]]; then
        cost="0.00"
    fi

    # Normalize zero costs to "0.00" format for consistency
    # Check if cost is effectively zero (handles 0, 0.0, 0.00, 0.000000, etc.)
    if awk "BEGIN {exit !($cost == 0)}" 2>/dev/null; then
        cost="0.00"
    fi

    echo "$cost"
}

# Calculate cost from token result string
# Usage: calculate_cost_from_result <agent_name> <token_result>
# token_result format: "input_tokens|output_tokens"
calculate_cost_from_result() {
    local agent="${1:-unknown}"
    local token_result="$2"

    if [[ -z "$token_result" ]]; then
        echo "0.00"
        return 0
    fi

    local input_tokens output_tokens
    IFS='|' read -r input_tokens output_tokens <<< "$token_result"

    calculate_cost "$agent" "${input_tokens:-0}" "${output_tokens:-0}"
}

# -----------------------------------------------------------------------------
# Daily Spend Tracking
# -----------------------------------------------------------------------------

# Get today's date in YYYY-MM-DD format
_needle_effort_today() {
    date +%Y-%m-%d
}

# Get daily spend file path
_needle_effort_spend_file() {
    echo "${NEEDLE_DAILY_SPEND_FILE:-$NEEDLE_HOME/$NEEDLE_STATE_DIR/daily_spend.json}"
}

# Initialize daily spend file if needed
_needle_ensure_spend_file() {
    local spend_file
    spend_file=$(_needle_effort_spend_file)

    local spend_dir
    spend_dir=$(dirname "$spend_file")

    if [[ ! -d "$spend_dir" ]]; then
        mkdir -p "$spend_dir" || return 1
    fi

    if [[ ! -f "$spend_file" ]]; then
        echo '{}' > "$spend_file"
    fi
}

# Get daily spend totals
# Usage: _needle_get_daily_spend [date]
# Returns: JSON object with agents and totals
_needle_get_daily_spend() {
    local date="${1:-$(_needle_effort_today)}"
    local spend_file
    spend_file=$(_needle_effort_spend_file)

    _needle_ensure_spend_file || return 1

    if command -v jq &>/dev/null; then
        jq ".[\"$date\"] // {}" "$spend_file" 2>/dev/null
    else
        # Fallback: simple grep/parse
        python3 -c "
import json
import sys

try:
    with open('$spend_file', 'r') as f:
        data = json.load(f)
    print(json.dumps(data.get('$date', {})))
except:
    print('{}')
" 2>/dev/null
    fi
}

# Get total spend for a specific day
# Usage: _needle_get_total_spend [date]
# Returns: Total cost for the day
_needle_get_total_spend() {
    local date="${1:-$(_needle_effort_today)}"

    local daily_spend
    daily_spend=$(_needle_get_daily_spend "$date")

    if command -v jq &>/dev/null; then
        jq -r '.total // 0' <<< "$daily_spend" 2>/dev/null
    else
        python3 -c "
import json
try:
    data = json.loads('''$daily_spend''')
    print(data.get('total', 0))
except:
    print(0)
" 2>/dev/null
    fi
}

# Record effort (cost) for a bead
# Usage: record_effort <bead_id> <cost> [agent_name] [input_tokens] [output_tokens] [strand] [bead_type]
# Updates daily spend file atomically and emits telemetry event
#
# Arguments:
#   bead_id       - Required: Bead identifier (e.g., nd-abc123)
#   cost          - Required: Cost in USD (e.g., "0.0125")
#   agent_name    - Optional: Agent name (default: unknown)
#   input_tokens  - Optional: Input token count (default: 0)
#   output_tokens - Optional: Output token count (default: 0)
#   strand        - Optional: Strand name (e.g., pluck, weave, pulse)
#   bead_type     - Optional: Bead type (e.g., task, feature, bug)
#
# Example:
#   record_effort "nd-abc123" "0.0125" "claude-anthropic-sonnet" 1000 500 "pluck" "task"
record_effort() {
    local bead_id="${1:-}"
    local cost="${2:-0}"
    local agent="${3:-unknown}"
    local input_tokens="${4:-0}"
    local output_tokens="${5:-0}"
    local strand="${6:-}"
    local bead_type="${7:-}"

    if [[ -z "$bead_id" ]]; then
        _needle_warn "Cannot record effort: bead_id required"
        return 1
    fi

    # Initialize if needed
    _needle_effort_init

    local today
    today=$(_needle_effort_today)

    local spend_file
    spend_file=$(_needle_effort_spend_file)

    # Ensure spend file exists
    _needle_ensure_spend_file || return 1

    # Update daily spend atomically
    if command -v jq &>/dev/null; then
        # Use jq for atomic update
        local tmp_file
        tmp_file=$(mktemp)

        jq --arg date "$today" \
           --arg cost "$cost" \
           --arg bead "$bead_id" \
           --arg agent "$agent" \
           --argjson in_tok "$input_tokens" \
           --argjson out_tok "$output_tokens" \
           --arg strand "$strand" \
           --arg bead_type "$bead_type" '
            .[$date] = (.[$date] // {}) |
            .[$date].total = ((.[$date].total // 0) + ($cost | tonumber)) |
            .[$date].agents = (.[$date].agents // {}) |
            .[$date].agents[$agent] = ((.[$date].agents[$agent] // 0) + ($cost | tonumber)) |
            .[$date].beads = (.[$date].beads // {}) |
            .[$date].beads[$bead] = (
                {
                    cost: ($cost | tonumber),
                    agent: $agent,
                    input_tokens: $in_tok,
                    output_tokens: $out_tok,
                    timestamp: (now | todate)
                }
                + (if $strand != "" then {strand: $strand} else {} end)
                + (if $bead_type != "" then {type: $bead_type} else {} end)
            )
        ' "$spend_file" > "$tmp_file" 2>/dev/null

        if [[ $? -eq 0 ]] && [[ -s "$tmp_file" ]]; then
            mv "$tmp_file" "$spend_file"
        else
            rm -f "$tmp_file"
            _needle_error "Failed to update daily spend file"
            return 1
        fi
    else
        # Fallback: use Python for atomic update
        python3 - "$cost" "$today" "$bead_id" "$agent" "$input_tokens" "$output_tokens" "$strand" "$bead_type" "$spend_file" << 'PYEOF' 2>/dev/null
import json
import os
import sys
import tempfile
from datetime import datetime

try:
    cost_val = float(sys.argv[1])
    date = sys.argv[2]
    bead = sys.argv[3]
    agent = sys.argv[4]
    in_tok = int(sys.argv[5])
    out_tok = int(sys.argv[6])
    strand = sys.argv[7] if len(sys.argv) > 7 and sys.argv[7] else None
    bead_type = sys.argv[8] if len(sys.argv) > 8 and sys.argv[8] else None
    spend_file = sys.argv[9]

    # Read existing data
    with open(spend_file, 'r') as f:
        data = json.load(f)

    # Initialize date entry if needed
    if date not in data:
        data[date] = {'total': 0, 'agents': {}, 'beads': {}}

    # Update totals
    data[date]['total'] = data[date].get('total', 0) + cost_val
    data[date]['agents'][agent] = data[date]['agents'].get(agent, 0) + cost_val

    # Record bead details
    bead_record = {
        'cost': cost_val,
        'agent': agent,
        'input_tokens': in_tok,
        'output_tokens': out_tok,
        'timestamp': datetime.utcnow().isoformat() + 'Z'
    }
    if strand:
        bead_record['strand'] = strand
    if bead_type:
        bead_record['type'] = bead_type
    data[date]['beads'][bead] = bead_record

    # Atomic write
    fd, tmp_path = tempfile.mkstemp(dir=os.path.dirname(spend_file))
    with os.fdopen(fd, 'w') as f:
        json.dump(data, f, indent=2)
    os.replace(tmp_path, spend_file)

except Exception as e:
    print(f'Error: {e}', file=sys.stderr)
    sys.exit(1)
PYEOF

        if [[ $? -ne 0 ]]; then
            _needle_error "Failed to update daily spend file"
            return 1
        fi
    fi

    _needle_debug "Recorded effort: bead=$bead_id, cost=\$$cost, agent=$agent, strand=${strand:-}, type=${bead_type:-}"

    # Emit effort.recorded telemetry event (include strand/type if available)
    local effort_args=("bead_id=$bead_id" "cost=$cost" "agent=$agent" "input_tokens=$input_tokens" "output_tokens=$output_tokens")
    if [[ -n "$strand" ]]; then
        effort_args+=("strand=$strand")
    fi
    if [[ -n "$bead_type" ]]; then
        effort_args+=("type=$bead_type")
    fi
    _needle_telemetry_emit "effort.recorded" "info" "${effort_args[@]}"

    return 0
}

# Record effort from token extraction result
# Usage: record_effort_from_tokens <bead_id> <agent_name> <token_result>
# token_result format: "input_tokens|output_tokens"
record_effort_from_tokens() {
    local bead_id="${1:-}"
    local agent="${2:-unknown}"
    local token_result="${3:-0|0}"

    if [[ -z "$bead_id" ]]; then
        _needle_warn "Cannot record effort: bead_id required"
        return 1
    fi

    local input_tokens output_tokens
    IFS='|' read -r input_tokens output_tokens <<< "$token_result"

    # Calculate cost
    local cost
    cost=$(calculate_cost "$agent" "${input_tokens:-0}" "${output_tokens:-0}")

    # Record the effort
    record_effort "$bead_id" "$cost" "$agent" "${input_tokens:-0}" "${output_tokens:-0}"
}

# -----------------------------------------------------------------------------
# Spend Reporting
# -----------------------------------------------------------------------------

# Get spend summary for a date range
# Usage: _needle_get_spend_summary [start_date] [end_date]
# Dates in YYYY-MM-DD format, defaults to today
_needle_get_spend_summary() {
    local start_date="${1:-$(_needle_effort_today)}"
    local end_date="${2:-$start_date}"
    local spend_file
    spend_file=$(_needle_effort_spend_file)

    _needle_ensure_spend_file || return 1

    if command -v jq &>/dev/null; then
        jq --arg start "$start_date" --arg end "$end_date" '
            to_entries |
            map(select(.key >= $start and .key <= $end)) |
            {
                total: (map(.value.total // 0) | add),
                days: length,
                by_agent: (map(.value.agents // {}) | add | to_entries | map({key: .key, value: .value}) | from_entries),
                daily: from_entries
            }
        ' "$spend_file" 2>/dev/null
    else
        python3 -c "
import json
from datetime import datetime

try:
    with open('$spend_file', 'r') as f:
        data = json.load(f)

    start = '$start_date'
    end = '$end_date'

    total = 0
    agents = {}
    daily = {}
    days = 0

    for date, entry in data.items():
        if start <= date <= end:
            total += entry.get('total', 0)
            days += 1
            daily[date] = entry

            for agent, cost in entry.get('agents', {}).items():
                agents[agent] = agents.get(agent, 0) + cost

    result = {
        'total': total,
        'days': days,
        'by_agent': agents,
        'daily': daily
    }
    print(json.dumps(result, indent=2))
except Exception as e:
    print('{}')
" 2>/dev/null
    fi
}

# Display spend summary
# Usage: _needle_show_spend_summary [days]
_needle_show_spend_summary() {
    local days="${1:-1}"

    local start_date
    if [[ "$days" -gt 1 ]]; then
        start_date=$(date -d "$days days ago" +%Y-%m-%d 2>/dev/null || date -v-${days}d +%Y-%m-%d)
    else
        start_date=$(_needle_effort_today)
    fi

    local end_date
    end_date=$(_needle_effort_today)

    local summary
    summary=$(_needle_get_spend_summary "$start_date" "$end_date")

    if command -v jq &>/dev/null; then
        local total days_count
        total=$(jq -r '.total // 0' <<< "$summary")
        days_count=$(jq -r '.days // 0' <<< "$summary")

        _needle_header "Spend Summary ($days_count days)"
        _needle_table_row "Total Spend" "\$$total"

        # Show by agent
        local by_agent
        by_agent=$(jq -r '.by_agent // {} | to_entries | .[] | "  \(.key): \$\(.value)"' <<< "$summary" 2>/dev/null)
        if [[ -n "$by_agent" ]]; then
            _needle_section "By Agent"
            echo "$by_agent"
        fi
    else
        echo "$summary"
    fi
}

# -----------------------------------------------------------------------------
# Telemetry Events
# -----------------------------------------------------------------------------

# Emit effort.recorded telemetry event
# Usage: _needle_event_effort_recorded <bead_id> <cost> <agent> <input_tokens> <output_tokens>
_needle_event_effort_recorded() {
    local bead_id="$1"
    local cost="$2"
    local agent="$3"
    local input_tokens="${4:-0}"
    local output_tokens="${5:-0}"

    # Source events module if not loaded
    if ! declare -f _needle_telemetry_emit &>/dev/null; then
        local events_path
        events_path="$(dirname "${BASH_SOURCE[0]}")/events.sh"
        if [[ -f "$events_path" ]]; then
            source "$events_path"
        fi
    fi

    # Emit the event
    if declare -f _needle_telemetry_emit &>/dev/null; then
        _needle_telemetry_emit "effort.recorded" "info" \
            "bead_id=$bead_id" \
            "cost=$cost" \
            "agent=$agent" \
            "input_tokens=$input_tokens" \
            "output_tokens=$output_tokens"
    fi
}

# -----------------------------------------------------------------------------
# Per-Bead Effort Lookup (join events.jsonl to bead on close)
# -----------------------------------------------------------------------------

# Get aggregated effort for a specific bead from NEEDLE session logs
# Usage: _needle_get_bead_effort <bead_id>
# Returns: input_tokens|output_tokens|cost_usd|attempts|agents
#
# Reads all NEEDLE session log files ($NEEDLE_HOME/logs/*.log|*.jsonl) and sums
# effort events (effort.recorded and bead.effort_recorded) for the given bead.
# This gives accurate totals across multiple agent attempts on the same bead.
#
# The 5th field (agents) is a comma-separated list of unique worker/agent names
# that contributed effort, enabling "join events.jsonl to bead assignee" queries.
#
# Example:
#   result=$(_needle_get_bead_effort "nd-abc123")
#   IFS='|' read -r in out cost attempts agents <<< "$result"
#   echo "Cost: \$$cost ($in in / $out out, $attempts attempts by $agents)"
_needle_get_bead_effort() {
    local bead_id="$1"

    if [[ -z "$bead_id" ]]; then
        echo "0|0|0|0|"
        return 1
    fi

    # Resolve log directory: use NEEDLE_LOG_DIR if absolute, else join with NEEDLE_HOME
    local log_dir
    if [[ "${NEEDLE_LOG_DIR:-}" == /* ]]; then
        log_dir="$NEEDLE_LOG_DIR"
    else
        log_dir="${NEEDLE_HOME:-$HOME/.needle}/${NEEDLE_LOG_DIR:-logs}"
    fi

    if [[ ! -d "$log_dir" ]]; then
        echo "0|0|0|0|"
        return 0
    fi

    # Collect all session log files (both .log and .jsonl extensions)
    local log_files=()
    while IFS= read -r f; do
        log_files+=("$f")
    done < <(find "$log_dir" -maxdepth 1 \( -name "*.log" -o -name "*.jsonl" \) -type f 2>/dev/null)

    if [[ ${#log_files[@]} -eq 0 ]]; then
        echo "0|0|0|0|"
        return 0
    fi

    if command -v jq &>/dev/null; then
        # Parse all log files and aggregate effort events for this bead.
        # Two-stage pipeline: first filter to valid JSON objects (skipping
        # malformed lines), then aggregate effort events for the target bead.
        cat "${log_files[@]}" 2>/dev/null | \
            jq -Rrc 'try fromjson catch empty' 2>/dev/null | \
            jq -rcs --arg bead_id "$bead_id" '
                [.[] | select(
                    (.event == "effort.recorded" or .event == "bead.effort_recorded")
                    and .data.bead_id == $bead_id
                )] |
                {
                    attempts: length,
                    input_tokens: (map(.data.input_tokens // 0) | add // 0),
                    output_tokens: (map(.data.output_tokens // 0) | add // 0),
                    cost_usd: (map(.data.cost // "0" | tonumber) | add // 0),
                    agents: (map(.data.agent // .worker // "") | map(select(length > 0)) | unique | join(","))
                } |
                "\(.input_tokens)|\(.output_tokens)|\(.cost_usd)|\(.attempts)|\(.agents)"
            ' 2>/dev/null || echo "0|0|0|0|"
    else
        # Fallback: python3
        python3 - "$bead_id" "$log_dir" << 'PYEOF' 2>/dev/null || echo "0|0|0|0|"
import json, sys, glob, os

bead_id = sys.argv[1]
log_dir = sys.argv[2]

total_input, total_output, total_cost, attempts = 0, 0, 0.0, 0
agents_seen = []

for pattern in ('*.log', '*.jsonl'):
    for log_file in glob.glob(os.path.join(log_dir, pattern)):
        try:
            with open(log_file, 'r') as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        ev = json.loads(line)
                        if ev.get('event') in ('effort.recorded', 'bead.effort_recorded'):
                            d = ev.get('data', {})
                            if d.get('bead_id') == bead_id:
                                total_input += int(d.get('input_tokens', 0) or 0)
                                total_output += int(d.get('output_tokens', 0) or 0)
                                try:
                                    total_cost += float(d.get('cost', '0') or '0')
                                except (ValueError, TypeError):
                                    pass
                                attempts += 1
                                agent = d.get('agent', '') or ev.get('worker', '')
                                if agent and agent not in agents_seen:
                                    agents_seen.append(agent)
                    except json.JSONDecodeError:
                        pass
        except IOError:
            pass

agents_str = ','.join(agents_seen)
print(f'{total_input}|{total_output}|{total_cost:.6f}|{attempts}|{agents_str}')
PYEOF
    fi
}

# Annotate a closed bead with its effort/cost summary from session logs
# Usage: _needle_annotate_bead_with_effort <bead_id> [workspace]
# Returns: 0 if annotation was written, 1 otherwise
#
# Reads NEEDLE session logs, sums all effort events for the given bead, then
# adds a cost comment to the bead record via `br comments add`. Safe to call
# even if no effort data exists — it silently skips annotation in that case.
#
# Example:
#   _needle_annotate_bead_with_effort "nd-abc123" "/home/user/project"
_needle_annotate_bead_with_effort() {
    local bead_id="$1"
    local workspace="${2:-}"

    if [[ -z "$bead_id" ]]; then
        return 1
    fi

    # Get aggregated effort from session logs
    local effort_result
    effort_result=$(_needle_get_bead_effort "$bead_id")

    local input_tokens output_tokens cost_usd attempts agents
    IFS='|' read -r input_tokens output_tokens cost_usd attempts agents <<< "$effort_result"

    input_tokens="${input_tokens:-0}"
    output_tokens="${output_tokens:-0}"
    cost_usd="${cost_usd:-0}"
    attempts="${attempts:-0}"
    agents="${agents:-}"

    # Skip if nothing was recorded
    if [[ "$attempts" -eq 0 ]] && [[ "$input_tokens" -eq 0 ]]; then
        _needle_debug "No effort data found for bead $bead_id, skipping annotation"
        return 0
    fi

    local total_tokens=$(( input_tokens + output_tokens ))

    # Format cost string
    local cost_display
    cost_display=$(printf "%.6f" "$cost_usd" 2>/dev/null || echo "$cost_usd")

    # Build comment — include worker attribution when available
    local comment
    if [[ -n "$agents" ]]; then
        comment="**Cost attribution** | worker: ${agents} | cost: \$${cost_display} | input: ${input_tokens} | output: ${output_tokens} | total: ${total_tokens} tokens | attempts: ${attempts}"
    else
        comment="**Cost attribution** | cost: \$${cost_display} | input: ${input_tokens} | output: ${output_tokens} | total: ${total_tokens} tokens | attempts: ${attempts}"
    fi

    # Write comment to bead via br
    if ! _needle_command_exists br; then
        _needle_debug "br not found, cannot annotate bead $bead_id with cost data"
        return 1
    fi

    local br_rc=0
    if [[ -n "$workspace" ]] && [[ -d "$workspace" ]]; then
        (cd "$workspace" && br comments add "$bead_id" "$comment" 2>/dev/null) || br_rc=$?
    else
        br comments add "$bead_id" "$comment" 2>/dev/null || br_rc=$?
    fi

    if [[ $br_rc -eq 0 ]]; then
        _needle_debug "Annotated bead $bead_id: cost=\$$cost_display, tokens=${total_tokens}"
        return 0
    else
        _needle_warn "Failed to annotate bead $bead_id with cost data (br exit: $br_rc)"
        return 1
    fi
}

# -----------------------------------------------------------------------------
# Direct Execution Support (for testing)
# -----------------------------------------------------------------------------

# Allow running this module directly for testing
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    # Source dependencies for standalone testing
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/output.sh"
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/constants.sh"
    source "$(dirname "${BASH_SOURCE[0]}")/../lib/utils.sh"

    case "${1:-}" in
        calculate)
            if [[ $# -lt 4 ]]; then
                echo "Usage: $0 calculate <agent> <input_tokens> <output_tokens>"
                exit 1
            fi
            calculate_cost "$2" "$3" "$4"
            ;;
        record)
            if [[ $# -lt 3 ]]; then
                echo "Usage: $0 record <bead_id> <cost> [agent] [input_tokens] [output_tokens] [strand] [bead_type]"
                exit 1
            fi
            record_effort "$2" "$3" "${4:-unknown}" "${5:-0}" "${6:-0}" "${7:-}" "${8:-}"
            ;;
        spend)
            _needle_show_spend_summary "${2:-1}"
            ;;
        daily)
            _needle_get_daily_spend "${2:-}"
            ;;
        summary)
            _needle_get_spend_summary "${2:-}" "${3:-}"
            ;;
        init)
            _needle_effort_init
            echo "Daily spend file: $NEEDLE_DAILY_SPEND_FILE"
            ;;
        get-effort)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 get-effort <bead_id>"
                exit 1
            fi
            _needle_get_bead_effort "$2"
            ;;
        annotate)
            if [[ -z "${2:-}" ]]; then
                echo "Usage: $0 annotate <bead_id> [workspace]"
                exit 1
            fi
            _needle_annotate_bead_with_effort "$2" "${3:-}"
            ;;
        -h|--help)
            echo "Usage: $0 <command> [args]"
            echo ""
            echo "Commands:"
            echo "  calculate <agent> <in> <out>   Calculate cost for tokens"
            echo "  record <bead> <cost> [agent] [in] [out]  Record effort"
            echo "  spend [days]                   Show spend summary"
            echo "  daily [date]                   Get daily spend JSON"
            echo "  summary [start] [end]          Get spend summary JSON"
            echo "  init                           Initialize spend file"
            echo "  get-effort <bead_id>           Get aggregated effort from logs"
            echo "  annotate <bead_id> [workspace] Annotate bead with cost comment"
            ;;
        *)
            echo "Unknown command: ${1:-}"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
fi

# Module loaded flag
_NEEDLE_EFFORT_LOADED="true"
