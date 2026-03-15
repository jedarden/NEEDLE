#!/usr/bin/env bash
# NEEDLE CLI Analyze Subcommand
# Analyze codebase patterns and file contention

_needle_analyze_help() {
    _needle_print "Analyze codebase patterns and file contention

USAGE:
    needle analyze <SUBCOMMAND> [OPTIONS]

SUBCOMMANDS:
    hot-files    Identify frequently contested files and optionally create refactoring beads
    cost         Report per-bead and per-agent token costs from effort tracking

OPTIONS:
    -h, --help   Show this help message

EXAMPLES:
    needle analyze hot-files
    needle analyze hot-files --top=5
    needle analyze hot-files --create-beads
    needle analyze hot-files --period=7d --create-beads
    needle analyze cost
    needle analyze cost --period=7d
    needle analyze cost --bead=nd-abc123
    needle analyze cost --agent=claude-anthropic-sonnet
    needle analyze cost --top=10 --json
"
}

_needle_analyze() {
    local subcommand="${1:-}"
    shift || true

    case "$subcommand" in
        hot-files|hot_files)
            _needle_analyze_hot_files "$@"
            ;;
        cost)
            _needle_analyze_cost "$@"
            ;;
        -h|--help|help|"")
            _needle_analyze_help
            exit $NEEDLE_EXIT_SUCCESS
            ;;
        *)
            _needle_error "Unknown subcommand: $subcommand"
            _needle_analyze_help
            exit $NEEDLE_EXIT_USAGE
            ;;
    esac
}

# ----------------------------------------------------------------------------
# needle analyze hot-files [--create-beads]
# ----------------------------------------------------------------------------

_needle_analyze_hot_files() {
    local top=10
    local period="7d"
    local create_beads=false
    local json_output=false
    local workspace=""
    local min_conflicts=3

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --top=*)
                top="${1#*=}"
                shift
                ;;
            --top)
                top="$2"
                shift 2
                ;;
            --period=*)
                period="${1#*=}"
                shift
                ;;
            --period)
                period="$2"
                shift 2
                ;;
            --create-beads)
                create_beads=true
                shift
                ;;
            --min-conflicts=*)
                min_conflicts="${1#*=}"
                shift
                ;;
            --min-conflicts)
                min_conflicts="$2"
                shift 2
                ;;
            --workspace=*)
                workspace="${1#*=}"
                shift
                ;;
            --workspace|-w)
                workspace="$2"
                shift 2
                ;;
            -j|--json)
                json_output=true
                shift
                ;;
            -h|--help)
                _needle_print "Identify frequently contested files

USAGE:
    needle analyze hot-files [OPTIONS]

OPTIONS:
    --top=N                  Show top N files (default: 10)
    --period=<duration>      Analysis period (default: 7d)
    --create-beads           Auto-generate refactoring beads for hot files
    --min-conflicts=N        Minimum conflicts to qualify (default: 3, for --create-beads)
    --workspace=<path>       Workspace context for bead creation
    -j, --json               Output JSON
    -h, --help               Show this help
"
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                _needle_error "Unknown option: $1"
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    workspace="${workspace:-$(pwd)}"

    source "$NEEDLE_ROOT_DIR/src/lock/metrics.sh"

    local metrics
    metrics=$(_needle_metrics_aggregate "$period")

    local hot_files
    hot_files=$(echo "$metrics" | jq --argjson top "$top" '.hot_files[0:$top]')

    local count
    count=$(echo "$hot_files" | jq 'length')

    if [[ "$json_output" == "true" ]] && [[ "$create_beads" != "true" ]]; then
        echo "$hot_files"
        return
    fi

    if [[ "$count" -eq 0 ]]; then
        _needle_info "No hot files detected in the last ${period}"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    _needle_header "Hot Files Analysis (${period})"
    _needle_print ""

    local max_conflicts
    max_conflicts=$(echo "$hot_files" | jq '.[0].conflicts // 1')
    if (( max_conflicts < 1 )); then max_conflicts=1; fi

    echo "$hot_files" | jq -r '.[] | "\(.conflicts)\t\(.path)"' | \
    while IFS=$'\t' read -r conflicts path; do
        local bar_len=$(( conflicts * 20 / max_conflicts ))
        if (( bar_len < 1 )); then bar_len=1; fi
        local bar="" i=0
        while (( i < bar_len )); do bar+="█"; ((i++)); done
        printf "  %3dx  %-20s  %s\n" "$conflicts" "$bar" "$path"
    done
    _needle_print ""

    if [[ "$create_beads" == "true" ]]; then
        if ! _needle_command_exists br; then
            _needle_warn "br command not found, cannot create beads"
            exit $NEEDLE_EXIT_DEPENDENCY
        fi

        _needle_info "Creating refactoring beads for hot files (min conflicts: ${min_conflicts})..."
        _needle_print ""

        local created=0 skipped=0

        while IFS=$'\t' read -r conflicts path; do
            if (( conflicts < min_conflicts )); then
                skipped=$(( skipped + 1 ))
                continue
            fi

            local filename
            filename=$(basename "$path")
            local title="Refactor: reduce contention in ${filename}"

            local description
            description="## Hot File: \`${filename}\`

File \`${path}\` has been contested **${conflicts} times** in the last ${period}.

This file is a hot spot for concurrent access conflicts between workers.

## Why This Matters

High contention means multiple workers are frequently trying to edit the same file simultaneously, leading to:
- Worker blocking (one must wait for the other to finish)
- Potential merge conflicts if checkout system is bypassed
- Reduced parallelism and throughput

## Suggested Refactoring Approaches

1. **Split into smaller modules** — Break the file into focused submodules so workers edit different files
2. **Extract shared utilities** — Move shared code to a lib/ file that rarely changes
3. **Add a facade layer** — Create an interface that delegates to specialized implementations
4. **Reduce scope** — Narrow what each bead needs to change in this file

## Metrics

- Contention count: ${conflicts} (period: ${period})
- Workspace: ${workspace}

---
*Auto-generated by: \`needle analyze hot-files --create-beads\`*

## Required for
/home/coder/NEEDLE bead nd-2a0i (needle metrics: File collision analytics)"

            local bead_id
            bead_id=$(br create \
                --title "$title" \
                --description "$description" \
                --add-label refactor \
                --add-label hot-file \
                2>/dev/null | grep -oP 'Created issue \K[a-z0-9-]+' || true)

            if [[ -n "$bead_id" ]]; then
                _needle_success "Created bead $bead_id: $title"
                created=$(( created + 1 ))
            else
                _needle_warn "Failed to create bead for: $path"
            fi
        done < <(echo "$hot_files" | jq -r '.[] | "\(.conflicts)\t\(.path)"')

        _needle_print ""
        if (( created > 0 )); then
            _needle_success "Created $created refactoring bead(s)"
        else
            _needle_info "No beads created (all files below min-conflicts threshold of ${min_conflicts})"
        fi
        if (( skipped > 0 )); then
            _needle_info "Skipped $skipped files below threshold"
        fi
    fi

    exit $NEEDLE_EXIT_SUCCESS
}

# ----------------------------------------------------------------------------
# needle analyze cost [--period=7d] [--bead=<id>] [--agent=<name>] [--top=N] [--json]
# ----------------------------------------------------------------------------

_needle_analyze_cost() {
    local period="7d"
    local bead_filter=""
    local agent_filter=""
    local top=20
    local json_output=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --period=*)
                period="${1#*=}"
                shift
                ;;
            --period)
                period="$2"
                shift 2
                ;;
            --bead=*)
                bead_filter="${1#*=}"
                shift
                ;;
            --bead)
                bead_filter="$2"
                shift 2
                ;;
            --agent=*)
                agent_filter="${1#*=}"
                shift
                ;;
            --agent)
                agent_filter="$2"
                shift 2
                ;;
            --top=*)
                top="${1#*=}"
                shift
                ;;
            --top)
                top="$2"
                shift 2
                ;;
            -j|--json)
                json_output=true
                shift
                ;;
            -h|--help)
                _needle_print "Report per-bead and per-agent token costs

USAGE:
    needle analyze cost [OPTIONS]

OPTIONS:
    --period=<duration>   Analysis period: Nd (days), Nw (weeks), all (default: 7d)
    --bead=<id>           Show cost for a specific bead ID
    --agent=<name>        Filter by agent name
    --top=N               Show top N beads by cost (default: 20)
    -j, --json            Output JSON
    -h, --help            Show this help

EXAMPLES:
    needle analyze cost
    needle analyze cost --period=30d
    needle analyze cost --bead=nd-abc123
    needle analyze cost --agent=claude-anthropic-sonnet --period=7d
    needle analyze cost --top=5 --json
"
                exit $NEEDLE_EXIT_SUCCESS
                ;;
            *)
                _needle_error "Unknown option: $1"
                exit $NEEDLE_EXIT_USAGE
                ;;
        esac
    done

    # Source effort module to get spend file path
    source "$NEEDLE_ROOT_DIR/src/telemetry/effort.sh"

    local spend_file
    spend_file=$(_needle_effort_spend_file)

    if [[ ! -f "$spend_file" ]]; then
        _needle_info "No cost data found. Run workers to accumulate cost data."
        _needle_info "Expected file: $spend_file"
        exit $NEEDLE_EXIT_SUCCESS
    fi

    # Parse period into a start date
    local start_date end_date
    end_date=$(date +%Y-%m-%d)

    if [[ "$period" == "all" ]]; then
        start_date="0000-01-01"
    else
        local num_days=7
        if [[ "$period" =~ ^([0-9]+)d$ ]]; then
            num_days="${BASH_REMATCH[1]}"
        elif [[ "$period" =~ ^([0-9]+)w$ ]]; then
            num_days=$(( ${BASH_REMATCH[1]} * 7 ))
        fi
        start_date=$(date -d "$num_days days ago" +%Y-%m-%d 2>/dev/null || \
                     date -v-${num_days}d +%Y-%m-%d 2>/dev/null || \
                     echo "0000-01-01")
    fi

    # Build report using Python (handles jq absence gracefully)
    local report
    report=$(python3 - "$spend_file" "$start_date" "$end_date" "$bead_filter" "$agent_filter" "$top" "$json_output" <<'PYEOF'
import json
import sys
import os

spend_file = sys.argv[1]
start_date = sys.argv[2]
end_date = sys.argv[3]
bead_filter = sys.argv[4]
agent_filter = sys.argv[5]
top = int(sys.argv[6])
json_out = sys.argv[7] == "true"

try:
    with open(spend_file, 'r') as f:
        data = json.load(f)
except Exception as e:
    print(f"Error reading spend file: {e}", file=sys.stderr)
    sys.exit(1)

# Aggregate across date range — derive all agent costs from bead-level records
total_cost = 0.0
total_input_tokens = 0
total_output_tokens = 0
agents = {}   # agent -> {cost, input_tokens, output_tokens, bead_count}
beads = {}    # bead_id -> {cost, agent, input_tokens, output_tokens, date}
days_seen = set()

for date, entry in sorted(data.items()):
    if date < start_date or date > end_date:
        continue

    for bead_id, bead_data in (entry.get('beads') or {}).items():
        if bead_filter and bead_id != bead_filter:
            continue
        bead_agent = bead_data.get('agent', 'unknown')
        if agent_filter and agent_filter not in bead_agent:
            continue

        bead_cost = bead_data.get('cost', 0) or 0
        bead_in = bead_data.get('input_tokens', 0) or 0
        bead_out = bead_data.get('output_tokens', 0) or 0

        days_seen.add(date)
        total_cost += bead_cost
        total_input_tokens += bead_in
        total_output_tokens += bead_out

        if bead_agent not in agents:
            agents[bead_agent] = {'cost': 0.0, 'input_tokens': 0, 'output_tokens': 0, 'bead_count': 0}
        agents[bead_agent]['cost'] += bead_cost
        agents[bead_agent]['input_tokens'] += bead_in
        agents[bead_agent]['output_tokens'] += bead_out
        agents[bead_agent]['bead_count'] += 1

        if bead_id in beads:
            beads[bead_id]['cost'] += bead_cost
            beads[bead_id]['input_tokens'] += bead_in
            beads[bead_id]['output_tokens'] += bead_out
        else:
            beads[bead_id] = {
                'cost': bead_cost,
                'agent': bead_agent,
                'input_tokens': bead_in,
                'output_tokens': bead_out,
                'date': date
            }

# Sort beads by cost descending
sorted_beads = sorted(beads.items(), key=lambda x: x[1]['cost'], reverse=True)[:top]
sorted_agents = sorted(agents.items(), key=lambda x: x[1]['cost'], reverse=True)

if json_out:
    result = {
        'period': {'start': start_date, 'end': end_date, 'days': len(days_seen)},
        'total': {
            'cost_usd': round(total_cost, 6),
            'input_tokens': total_input_tokens,
            'output_tokens': total_output_tokens,
            'bead_count': len(beads),
        },
        'by_agent': [
            {
                'agent': a,
                'cost_usd': round(d['cost'], 6),
                'input_tokens': d['input_tokens'],
                'output_tokens': d['output_tokens'],
                'bead_count': d['bead_count'],
            }
            for a, d in sorted_agents
        ],
        'by_bead': [
            {
                'bead_id': bid,
                'cost_usd': round(d['cost'], 6),
                'agent': d['agent'],
                'input_tokens': d['input_tokens'],
                'output_tokens': d['output_tokens'],
                'date': d['date'],
            }
            for bid, d in sorted_beads
        ],
    }
    print(json.dumps(result, indent=2))
else:
    print(f"Period: {start_date} to {end_date} ({len(days_seen)} days with data)")
    print(f"Total cost:    ${total_cost:.6f}")
    print(f"Total tokens:  {total_input_tokens:,} in / {total_output_tokens:,} out")
    print(f"Beads tracked: {len(beads)}")
    print()

    if sorted_agents:
        print("By agent:")
        for agent_name, d in sorted_agents:
            print(f"  {agent_name:<45}  ${d['cost']:.6f}  ({d['bead_count']} beads, {d['input_tokens']:,}in/{d['output_tokens']:,}out)")
        print()

    if sorted_beads:
        print(f"Top {min(top, len(sorted_beads))} beads by cost:")
        for bead_id, d in sorted_beads:
            print(f"  {bead_id:<12}  ${d['cost']:.6f}  {d['agent']:<40}  {d['input_tokens']:>8,}in/{d['output_tokens']:>8,}out  {d['date']}")

PYEOF
    )
    local py_exit=$?

    if [[ $py_exit -ne 0 ]]; then
        _needle_error "Failed to generate cost report"
        exit $NEEDLE_EXIT_ERROR
    fi

    echo "$report"
    exit $NEEDLE_EXIT_SUCCESS
}
