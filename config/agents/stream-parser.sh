#!/usr/bin/env bash
# stream-parser.sh - Claude Code stream-json output parser
#
# Reads JSONL events from stdin (claude --output-format stream-json)
# and converts them to human-readable terminal output.
#
# Event types handled:
#   system      - Session initialization
#   assistant   - Agent messages (text, tool_use)
#   user        - Tool results (tool_result)
#   result      - Final result with usage stats
#
# Usage:
#   claude --output-format stream-json ... | stream-parser.sh
#
# Environment:
#   NEEDLE_HEARTBEAT_CMD  - Command to run for heartbeat keepalive (optional)
#   NO_COLOR              - Disable color output if set
#   TERM                  - Terminal type (auto-detects color support)

# Color support detection
if [[ -n "${NO_COLOR:-}" ]] || [[ "${TERM:-}" == "dumb" ]]; then
    COLOR_RESET=""
    COLOR_BOLD=""
    COLOR_DIM=""
    COLOR_GREEN=""
    COLOR_BLUE=""
    COLOR_YELLOW=""
    COLOR_CYAN=""
    COLOR_MAGENTA=""
    COLOR_RED=""
else
    COLOR_RESET="\033[0m"
    COLOR_BOLD="\033[1m"
    COLOR_DIM="\033[2m"
    COLOR_GREEN="\033[0;32m"
    COLOR_BLUE="\033[0;34m"
    COLOR_YELLOW="\033[0;33m"
    COLOR_CYAN="\033[0;36m"
    COLOR_MAGENTA="\033[0;35m"
    COLOR_RED="\033[0;31m"
fi

# Track last heartbeat time
_last_heartbeat_time=0
_heartbeat_interval=30

# Emit heartbeat if NEEDLE_HEARTBEAT_CMD is set and interval has elapsed
_maybe_heartbeat() {
    if [[ -n "${NEEDLE_HEARTBEAT_CMD:-}" ]]; then
        local now
        now=$(date +%s 2>/dev/null || echo "0")
        if (( now - _last_heartbeat_time >= _heartbeat_interval )); then
            eval "$NEEDLE_HEARTBEAT_CMD" 2>/dev/null || true
            _last_heartbeat_time="$now"
        fi
    fi
}

# Format tool input for display (truncate if too long)
_format_tool_input() {
    local input="$1"
    local max_len=120
    # Remove newlines for single-line display
    input=$(echo "$input" | tr '\n' ' ' | sed 's/  */ /g')
    if [[ ${#input} -gt $max_len ]]; then
        echo "${input:0:$max_len}..."
    else
        echo "$input"
    fi
}

# Process each JSONL line
while IFS= read -r line; do
    [[ -z "$line" ]] && continue

    # Parse event type
    TYPE=$(printf '%s' "$line" | jq -r '.type // empty' 2>/dev/null)
    [[ -z "$TYPE" ]] && continue

    case "$TYPE" in
        system)
            SUBTYPE=$(printf '%s' "$line" | jq -r '.subtype // empty' 2>/dev/null)
            if [[ "$SUBTYPE" == "init" ]]; then
                SESSION_ID=$(printf '%s' "$line" | jq -r '.session_id // empty' 2>/dev/null)
                printf "${COLOR_DIM}[system] Session initialized%s${COLOR_RESET}\n" \
                    "${SESSION_ID:+ (${SESSION_ID:0:8}...)}"
            fi
            ;;

        assistant)
            # Extract content array from the message
            CONTENT=$(printf '%s' "$line" | jq -c '.message.content // []' 2>/dev/null)
            if [[ -z "$CONTENT" ]] || [[ "$CONTENT" == "[]" ]]; then
                continue
            fi

            # Process each content block
            CONTENT_LENGTH=$(printf '%s' "$CONTENT" | jq 'length' 2>/dev/null || echo "0")
            for (( i=0; i<CONTENT_LENGTH; i++ )); do
                BLOCK=$(printf '%s' "$CONTENT" | jq -c ".[$i]" 2>/dev/null)
                BLOCK_TYPE=$(printf '%s' "$BLOCK" | jq -r '.type // empty' 2>/dev/null)

                case "$BLOCK_TYPE" in
                    thinking)
                        THINKING=$(printf '%s' "$BLOCK" | jq -r '.thinking // empty' 2>/dev/null)
                        if [[ -n "$THINKING" ]]; then
                            # Show first line of thinking only (truncate)
                            FIRST_LINE=$(echo "$THINKING" | head -1)
                            printf "${COLOR_DIM}  ~ %s${COLOR_RESET}\n" "${FIRST_LINE:0:100}"
                        fi
                        ;;

                    text)
                        TEXT=$(printf '%s' "$BLOCK" | jq -r '.text // empty' 2>/dev/null)
                        if [[ -n "$TEXT" ]]; then
                            printf "${COLOR_RESET}%s\n" "$TEXT"
                        fi
                        ;;

                    tool_use)
                        TOOL_NAME=$(printf '%s' "$BLOCK" | jq -r '.name // empty' 2>/dev/null)
                        TOOL_INPUT=$(printf '%s' "$BLOCK" | jq -c '.input // {}' 2>/dev/null)

                        # Format tool display based on tool name
                        case "$TOOL_NAME" in
                            Bash)
                                CMD=$(printf '%s' "$TOOL_INPUT" | jq -r '.command // empty' 2>/dev/null)
                                printf "${COLOR_CYAN}  ▶ Bash:${COLOR_RESET} %s\n" \
                                    "$(_format_tool_input "$CMD")"
                                ;;
                            Read)
                                FILE=$(printf '%s' "$TOOL_INPUT" | jq -r '.file_path // empty' 2>/dev/null)
                                printf "${COLOR_BLUE}  ▶ Read:${COLOR_RESET} %s\n" "$FILE"
                                ;;
                            Edit|Write)
                                FILE=$(printf '%s' "$TOOL_INPUT" | jq -r '.file_path // empty' 2>/dev/null)
                                printf "${COLOR_YELLOW}  ▶ %s:${COLOR_RESET} %s\n" "$TOOL_NAME" "$FILE"
                                ;;
                            Glob|Grep)
                                PATTERN=$(printf '%s' "$TOOL_INPUT" | jq -r '.pattern // empty' 2>/dev/null)
                                printf "${COLOR_MAGENTA}  ▶ %s:${COLOR_RESET} %s\n" "$TOOL_NAME" \
                                    "$(_format_tool_input "$PATTERN")"
                                ;;
                            TodoWrite)
                                printf "${COLOR_GREEN}  ▶ TodoWrite${COLOR_RESET}\n"
                                ;;
                            Agent)
                                DESC=$(printf '%s' "$TOOL_INPUT" | jq -r '.description // empty' 2>/dev/null)
                                printf "${COLOR_MAGENTA}  ▶ Agent:${COLOR_RESET} %s\n" \
                                    "$(_format_tool_input "${DESC:-subagent}")"
                                ;;
                            *)
                                INPUT_STR=$(_format_tool_input "$TOOL_INPUT")
                                printf "${COLOR_CYAN}  ▶ %s:${COLOR_RESET} %s\n" "$TOOL_NAME" "$INPUT_STR"
                                ;;
                        esac
                        ;;
                esac
            done
            ;;

        user)
            # Tool results - usually noisy, show only errors
            CONTENT=$(printf '%s' "$line" | jq -c '.message.content // []' 2>/dev/null)
            CONTENT_LENGTH=$(printf '%s' "$CONTENT" | jq 'length' 2>/dev/null || echo "0")
            for (( i=0; i<CONTENT_LENGTH; i++ )); do
                BLOCK=$(printf '%s' "$CONTENT" | jq -c ".[$i]" 2>/dev/null)
                BLOCK_TYPE=$(printf '%s' "$BLOCK" | jq -r '.type // empty' 2>/dev/null)
                if [[ "$BLOCK_TYPE" == "tool_result" ]]; then
                    IS_ERROR=$(printf '%s' "$BLOCK" | jq -r '.is_error // false' 2>/dev/null)
                    if [[ "$IS_ERROR" == "true" ]]; then
                        ERROR_CONTENT=$(printf '%s' "$BLOCK" | jq -r '.content // empty' 2>/dev/null)
                        printf "${COLOR_RED}  ✗ Tool error:${COLOR_RESET} %s\n" \
                            "$(_format_tool_input "$ERROR_CONTENT")"
                    fi
                fi
            done
            ;;

        result)
            # Final result with usage statistics
            SUBTYPE=$(printf '%s' "$line" | jq -r '.subtype // empty' 2>/dev/null)
            COST=$(printf '%s' "$line" | jq -r '.cost_usd // empty' 2>/dev/null)
            DURATION=$(printf '%s' "$line" | jq -r '.duration_ms // empty' 2>/dev/null)
            INPUT_TOKENS=$(printf '%s' "$line" | jq -r '.usage.input_tokens // empty' 2>/dev/null)
            OUTPUT_TOKENS=$(printf '%s' "$line" | jq -r '.usage.output_tokens // empty' 2>/dev/null)

            # Format duration
            if [[ -n "$DURATION" ]]; then
                DURATION_S=$(( DURATION / 1000 ))
                DURATION_FMT="${DURATION_S}s"
            else
                DURATION_FMT="?"
            fi

            # Format cost
            if [[ -n "$COST" ]]; then
                COST_FMT="\$${COST}"
            else
                COST_FMT="?"
            fi

            # Format token counts
            if [[ -n "$INPUT_TOKENS" ]] && [[ -n "$OUTPUT_TOKENS" ]]; then
                TOKEN_FMT="${INPUT_TOKENS}↑ ${OUTPUT_TOKENS}↓"
            else
                TOKEN_FMT="?"
            fi

            if [[ "$SUBTYPE" == "success" ]]; then
                printf "${COLOR_GREEN}${COLOR_BOLD}══ Result:${COLOR_RESET} ${COLOR_GREEN}success${COLOR_RESET}"
            else
                printf "${COLOR_YELLOW}${COLOR_BOLD}══ Result:${COLOR_RESET} ${COLOR_YELLOW}%s${COLOR_RESET}" "${SUBTYPE:-done}"
            fi
            printf " ${COLOR_DIM}[%s, %s tokens, %s]${COLOR_RESET}\n" \
                "$DURATION_FMT" "$TOKEN_FMT" "$COST_FMT"
            ;;
    esac

    # Heartbeat keepalive
    _maybe_heartbeat

done
