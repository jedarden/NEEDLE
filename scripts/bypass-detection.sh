#!/usr/bin/env bash
# Shared bypass detection and logging helpers for the verification hooks.
#
# This file is sourced by definition-of-done.sh and the Git hooks.  It is kept
# separate so that the pre-commit and post-commit hooks use exactly the same
# event format and locking behavior.

# Treat only explicit opt-out values as enabled.  In particular, SKIP_CHECKS=0
# must not silently disable the verification gate.
needle_bypass_requested() {
    case "${SKIP_CHECKS:-}" in
        1|true|TRUE|yes|YES|on|ON)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

needle_bypass_pattern() {
    if [[ "${NEEDLE_BYPASS_ARGUMENT:-}" == "--no-verify" ]]; then
        printf '%s' '--no-verify'
    elif needle_bypass_requested; then
        printf 'SKIP_CHECKS=%s' "${SKIP_CHECKS}"
    else
        printf '%s' 'git commit --no-verify'
    fi
}

needle_lanes_csv() {
    case "${1:-fast}" in
        fast)
            printf '%s' 'fast'
            ;;
        slow)
            printf '%s' 'slow'
            ;;
        all)
            printf '%s' 'fast,slow'
            ;;
        *)
            printf '%s' "${1:-fast}"
            ;;
    esac
}

needle_json_quote() {
    local value="${1-}"
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    value="${value//$'\n'/\\n}"
    value="${value//$'\r'/\\r}"
    value="${value//$'\t'/\\t}"
    printf '"%s"' "$value"
}

needle_json_array() {
    local csv="${1-}"
    local value
    local first=true
    local output='['
    local values=()

    IFS=',' read -r -a values <<< "$csv"
    for value in "${values[@]}"; do
        [[ -n "$value" ]] || continue
        if [[ "$first" == true ]]; then
            first=false
        else
            output+=','
        fi
        output+="$(needle_json_quote "$value")"
    done
    output+=']'
    printf '%s' "$output"
}

needle_hostname() {
    hostname 2>/dev/null || uname -n 2>/dev/null || printf '%s' 'unknown'
}

needle_username() {
    id -un 2>/dev/null || printf '%s' "${USER:-unknown}"
}

needle_current_commit() {
    local commit
    commit="$(git rev-parse --verify HEAD 2>/dev/null)" || commit=''
    printf '%s' "${commit:-unknown}"
}

needle_json_event() {
    local timestamp="$1"
    local commit_sha="$2"
    local lanes_csv="$3"
    local pattern="$4"
    local reason="$5"
    local working_directory="$6"

    printf '{"timestamp":%s,"commit_sha":%s,"hostname":%s,"username":%s,"lanes_skipped":%s,"pattern":%s,"reason":%s,"working_directory":%s}' \
        "$(needle_json_quote "$timestamp")" \
        "$(needle_json_quote "$commit_sha")" \
        "$(needle_json_quote "$(needle_hostname)")" \
        "$(needle_json_quote "$(needle_username)")" \
        "$(needle_json_array "$lanes_csv")" \
        "$(needle_json_quote "$pattern")" \
        "$(needle_json_quote "$reason")" \
        "$(needle_json_quote "$working_directory")"
}

# Append one complete JSON object while holding an inter-process lock.  flock
# is used where available; mkdir is an atomic lock acquisition fallback for
# environments that do not provide the command.  The lock file itself is
# intentionally ignored by Git.
needle_append_bypass_event() {
    local repository_root="${REPO_ROOT:-}"
    local log_file
    local lock_file
    local record="$1"
    local lock_fd
    local status=0

    if [[ -z "$repository_root" ]]; then
        repository_root="$(git rev-parse --show-toplevel 2>/dev/null)" || return 1
    fi
    log_file="${NEEDLE_BYPASS_LOG:-${repository_root}/.beads/bypasses.jsonl}"
    lock_file="${log_file}.lock"

    if ! mkdir -p "$(dirname "$log_file")"; then
        return 1
    fi

    if command -v flock >/dev/null 2>&1; then
        if ! exec {lock_fd}>>"$lock_file"; then
            return 1
        fi
        if ! flock -x "$lock_fd"; then
            eval "exec ${lock_fd}>&-"
            return 1
        fi
        printf '%s\n' "$record" >> "$log_file" || status=1
        flock -u "$lock_fd" || status=1
        eval "exec ${lock_fd}>&-"
        return "$status"
    fi

    # mkdir(2) is atomic.  This path is primarily for minimal environments;
    # the bounded wait also prevents a stale lock from hanging a Git commit.
    local lock_dir="${lock_file}.d"
    local attempts=0
    while ! mkdir "$lock_dir" 2>/dev/null; do
        attempts=$((attempts + 1))
        if (( attempts >= 3000 )); then
            return 1
        fi
        sleep 0.01
    done
    printf '%s\n' "$record" >> "$log_file" || status=1
    rmdir "$lock_dir" 2>/dev/null || status=1
    return "$status"
}

needle_warn_bypass() {
    local pattern="$1"
    local lanes_csv="$2"
    printf '\n⚠ WARNING: Definition of Done bypass detected.\n' >&2
    printf '  Pattern: %s\n' "$pattern" >&2
    printf '  Verification lanes skipped: %s\n' "${lanes_csv//,/, }" >&2
    printf '  This bypass will be recorded in .beads/bypasses.jsonl.\n\n' >&2
}

needle_git_state_dir() {
    git rev-parse --git-path needle-verification-state 2>/dev/null
}

needle_index_state_path() {
    local tree_sha
    local parent
    tree_sha="$(git write-tree 2>/dev/null)" || return 1
    parent="$(git rev-parse --verify HEAD 2>/dev/null)" || parent=''
    printf '%s/%s-%s' "$(needle_git_state_dir)" "${parent:-root}" "$tree_sha"
}

needle_head_state_path() {
    local tree_sha
    local parent
    tree_sha="$(git rev-parse --verify HEAD^{tree} 2>/dev/null)" || return 1
    parent="$(git rev-list --parents -n 1 HEAD 2>/dev/null | awk '{print $2}')"
    printf '%s/%s-%s' "$(needle_git_state_dir)" "${parent:-root}" "$tree_sha"
}

needle_write_state() {
    local status="$1"
    local lanes_csv="$2"
    local pattern="${3-}"
    local path
    local state_dir
    local temporary

    path="$(needle_index_state_path)" || return 1
    state_dir="$(dirname "$path")"
    mkdir -p "$state_dir" || return 1
    temporary="$(mktemp "${path}.tmp.XXXXXX")" || return 1
    if ! {
        printf 'status=%s\n' "$status"
        printf 'lanes=%s\n' "$lanes_csv"
        printf 'pattern=%s\n' "$pattern"
    } > "$temporary"; then
        rm -f "$temporary"
        return 1
    fi
    if ! mv -f "$temporary" "$path"; then
        rm -f "$temporary"
        return 1
    fi
}

needle_mark_verified() {
    needle_write_state 'verified' "$(needle_lanes_csv "${1:-fast}")"
}

needle_mark_bypass() {
    needle_write_state 'bypass' "$1" "$2"
}

needle_clear_index_state() {
    local path
    path="$(needle_index_state_path)" || return 0
    rm -f "$path"
}

needle_process_post_commit() {
    local path
    local status=''
    local lanes_csv=''
    local pattern=''
    local key value
    local commit_sha
    local reason
    local record
    local working_directory

    path="$(needle_head_state_path)" || return 0
    if [[ -f "$path" ]]; then
        while IFS='=' read -r key value; do
            case "$key" in
                status) status="$value" ;;
                lanes) lanes_csv="$value" ;;
                pattern) pattern="$value" ;;
            esac
        done < "$path"
    fi

    if [[ "$status" == verified ]]; then
        rm -f "$path"
        return 0
    fi

    commit_sha="$(needle_current_commit)"
    working_directory="$(pwd -P)"
    if [[ "$status" == bypass ]]; then
        reason="Verification was explicitly skipped before commit"
        needle_warn_bypass "$pattern" "$lanes_csv"
    else
        # --no-verify prevents pre-commit from running, so there is no state
        # marker.  post-commit is therefore the first reliable observation.
        pattern='git commit --no-verify'
        lanes_csv='fast'
        reason='No pre-commit verification marker was found; commit likely used --no-verify (fast lane and checkpoint skipped)'
        needle_warn_bypass "$pattern" "$lanes_csv"
    fi

    record="$(needle_json_event "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$commit_sha" "$lanes_csv" "$pattern" "$reason" "$working_directory")"
    if needle_append_bypass_event "$record"; then
        if [[ -n "$path" && -f "$path" ]]; then
            rm -f "$path"
        fi
    else
        printf '⚠ WARNING: Unable to append bypass event to .beads/bypasses.jsonl\n' >&2
    fi
}
