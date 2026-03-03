#!/usr/bin/env bash
# NEEDLE Worker Naming Module
# Generate worker identifiers using NATO alphabet
#
# This module provides functions for:
# - Generating unique, human-readable worker identifiers
# - Finding the next available NATO name (not currently in use)
# - Supporting custom --id override
# - Handling exhaustion (26+ workers) with numeric suffix
# - Validating identifier format

# -----------------------------------------------------------------------------
# Identifier Validation
# -----------------------------------------------------------------------------

# Validate a custom identifier
# Arguments:
#   $1 - Identifier to validate
# Returns: 0 if valid, 1 if invalid
# Valid format: lowercase letter followed by lowercase letters, numbers, or hyphens
# Usage: if validate_identifier "alpha-1"; then ...
validate_identifier() {
    local id="$1"

    # Must not be empty
    if [[ -z "$id" ]]; then
        return 1
    fi

    # Must start with a letter, followed by letters, numbers, or hyphens
    # All lowercase
    [[ "$id" =~ ^[a-z][a-z0-9-]*$ ]]
}

# Validate identifier and print error message if invalid
# Arguments:
#   $1 - Identifier to validate
# Returns: 0 if valid, 1 if invalid (prints error to stderr)
# Usage: if validate_identifier_verbose "bad-ID"; then ...
validate_identifier_verbose() {
    local id="$1"

    if [[ -z "$id" ]]; then
        echo "error: identifier cannot be empty" >&2
        return 1
    fi

    if [[ ! "$id" =~ ^[a-z][a-z0-9-]*$ ]]; then
        echo "error: identifier '$id' must start with lowercase letter and contain only lowercase letters, numbers, or hyphens" >&2
        return 1
    fi

    return 0
}

# -----------------------------------------------------------------------------
# Identifier Generation
# -----------------------------------------------------------------------------

# Get the next available identifier for an agent
# Arguments:
#   $1 - Agent identifier (format: runner-provider-model, e.g., "claude-anthropic-sonnet")
# Returns: First unused NATO identifier, or numeric suffix if all 26 used
# Usage: identifier=$(get_next_identifier "claude-anthropic-sonnet")
get_next_identifier() {
    local agent="$1"

    if [[ -z "$agent" ]]; then
        echo "alpha"
        return 0
    fi

    # Build the session prefix pattern
    local prefix="needle-$agent-"

    # Find existing sessions with this prefix and extract their identifiers
    local used
    used=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep "^$prefix" | sed 's/.*-//' || true)

    # Find first unused NATO name
    local name
    for name in "${NEEDLE_NATO_ALPHABET[@]}"; do
        if ! echo "$used" | grep -qx "$name"; then
            echo "$name"
            return 0
        fi
    done

    # All 26 NATO names used - add numeric suffix
    local count
    count=$(echo "$used" | grep -c . 2>/dev/null || echo "0")
    # Use count + 1 to get next available number
    echo "alpha-$((count + 1))"
}

# Get the next available identifier given a list of already-used identifiers
# Arguments:
#   $1 - Space or newline separated list of used identifiers
# Returns: First unused NATO identifier, or numeric suffix if all 26 used
# Usage: identifier=$(get_next_identifier_from_list "alpha bravo charlie")
get_next_identifier_from_list() {
    local used="$1"

    # Find first unused NATO name
    local name
    for name in "${NEEDLE_NATO_ALPHABET[@]}"; do
        if ! echo "$used" | grep -qw "$name"; then
            echo "$name"
            return 0
        fi
    done

    # All 26 NATO names used - add numeric suffix
    local count
    count=$(echo "$used" | wc -w | tr -d ' ')
    echo "alpha-$((count + 1))"
}

# -----------------------------------------------------------------------------
# Custom ID Override Support
# -----------------------------------------------------------------------------

# Extract custom ID from arguments if --id is present
# Arguments:
#   $@ - Command line arguments to parse
# Returns: Custom ID if found, empty string if not found
# Usage: custom_id=$(parse_custom_id "--workspace" "/path" "--id" "custom-1")
parse_custom_id() {
    local args=("$@")
    local i

    for ((i = 0; i < ${#args[@]}; i++)); do
        case "${args[$i]}" in
            --id)
                # Next argument is the ID
                if [[ $((i + 1)) -lt ${#args[@]} ]]; then
                    echo "${args[$((i + 1))]}"
                    return 0
                fi
                ;;
            --id=*)
                # ID is after equals sign
                echo "${args[$i]#--id=}"
                return 0
                ;;
        esac
    done

    # No custom ID found
    return 0
}

# Get identifier with custom override support
# Arguments:
#   $1 - Agent identifier (format: runner-provider-model)
#   $2 - Custom ID override (optional, empty string for auto)
# Returns: Custom ID if provided and valid, otherwise next available
# Usage: identifier=$(get_identifier_with_override "claude-anthropic-sonnet" "custom-1")
get_identifier_with_override() {
    local agent="$1"
    local custom_id="$2"

    # If custom ID provided, validate and use it
    if [[ -n "$custom_id" ]]; then
        if validate_identifier "$custom_id"; then
            echo "$custom_id"
            return 0
        else
            _needle_warn "Invalid custom identifier '$custom_id', using auto-generated"
        fi
    fi

    # Fall back to auto-generated
    get_next_identifier "$agent"
}

# -----------------------------------------------------------------------------
# Utility Functions
# -----------------------------------------------------------------------------

# Check if an identifier is a NATO name
# Arguments:
#   $1 - Identifier to check
# Returns: 0 if it's a NATO name, 1 if not
# Usage: if is_nato_identifier "alpha"; then ...
is_nato_identifier() {
    local id="$1"

    local name
    for name in "${NEEDLE_NATO_ALPHABET[@]}"; do
        if [[ "$id" == "$name" ]]; then
            return 0
        fi
    done

    return 1
}

# Get the index of a NATO identifier (0-25)
# Arguments:
#   $1 - NATO identifier
# Returns: Index (0-25), or -1 if not a NATO name
# Usage: index=$(get_nato_index "charlie")  # Returns 2
get_nato_index() {
    local id="$1"
    local i=0

    for name in "${NEEDLE_NATO_ALPHABET[@]}"; do
        if [[ "$id" == "$name" ]]; then
            echo "$i"
            return 0
        fi
        ((i++))
    done

    echo "-1"
}

# Get NATO identifier by index
# Arguments:
#   $1 - Index (0-25)
# Returns: NATO name at that index, or empty if out of range
# Usage: name=$(get_nato_by_index 2)  # Returns "charlie"
get_nato_by_index() {
    local index="$1"

    if [[ "$index" -lt 0 ]] || [[ "$index" -ge 26 ]]; then
        return 1
    fi

    echo "${NEEDLE_NATO_ALPHABET[$index]}"
}

# Count used NATO identifiers for an agent
# Arguments:
#   $1 - Agent identifier (format: runner-provider-model)
# Returns: Number of used NATO identifiers
# Usage: count=$(count_used_identifiers "claude-anthropic-sonnet")
count_used_identifiers() {
    local agent="$1"

    if [[ -z "$agent" ]]; then
        echo "0"
        return 0
    fi

    local prefix="needle-$agent-"
    local used
    used=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep "^$prefix" | sed 's/.*-//' || true)

    if [[ -z "$used" ]]; then
        echo "0"
        return 0
    fi

    echo "$used" | wc -l | tr -d ' '
}

# List all available (unused) NATO identifiers for an agent
# Arguments:
#   $1 - Agent identifier (format: runner-provider-model)
# Returns: Space-separated list of available NATO names
# Usage: available=$(list_available_identifiers "claude-anthropic-sonnet")
list_available_identifiers() {
    local agent="$1"
    local available=""

    if [[ -z "$agent" ]]; then
        echo "${NEEDLE_NATO_ALPHABET[*]}"
        return 0
    fi

    local prefix="needle-$agent-"
    local used
    used=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep "^$prefix" | sed 's/.*-//' || true)

    local name
    for name in "${NEEDLE_NATO_ALPHABET[@]}"; do
        if ! echo "$used" | grep -qx "$name"; then
            available="$available $name"
        fi
    done

    # Trim leading space
    echo "${available# }"
}
