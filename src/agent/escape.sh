#!/usr/bin/env bash
# NEEDLE Prompt Escape Module
# Safely escape prompts for injection into bash invoke templates
#
# Prompts may contain quotes, backticks, dollar signs that could break
# or exploit bash. This module provides safe escaping functions.

# Escape prompt for heredoc usage
# For heredocs with quoted delimiter ('NEEDLE_PROMPT'), no escaping needed
# The content is treated as literal when the delimiter is quoted
#
# Usage: echo "$prompt" | escape_for_heredoc
escape_for_heredoc() {
    # For heredocs with quoted delimiter, content is literal
    # NEEDLE_PROMPT is quoted ('NEEDLE_PROMPT') so content is treated literally
    # No escaping needed - just pass through
    cat
}

# Escape prompt for single-quoted bash strings
# Replaces ' with '\'' to safely embed in single-quoted strings
#
# Usage: echo "$prompt" | escape_for_single_quotes
# Example: echo "Test's quote" | escape_for_single_quotes
#   Output: Test'\''s quote
escape_for_single_quotes() {
    # Replace single quote with: end quote, escaped quote, start quote
    # ' -> '\'' (end current string, add escaped quote, start new string)
    sed "s/'/'\\\\''/g"
}

# Escape prompt for double-quoted bash strings
# Escapes: $ ` " \ and !
#
# Usage: echo "$prompt" | escape_for_double_quotes
# Note: Use single quotes when possible for safer escaping
escape_for_double_quotes() {
    # Must escape: $ ` " \ and historically !
    # Order matters: backslash must be first to not double-escape
    sed -e 's/\\/\\\\/g' \
        -e 's/\$/\\$/g' \
        -e 's/`/\\`/g' \
        -e 's/"/\\"/g' \
        -e 's/!/\\!/g'
}

# Escape prompt for safe use in bash -c command strings
# This is for: bash -c 'command "embedded prompt"'
# Uses single quote escaping strategy
#
# Usage: echo "$prompt" | escape_for_bash_c
escape_for_bash_c() {
    # For bash -c, we typically use single quotes for the outer string
    escape_for_single_quotes
}

# Escape prompt for JSON string embedding
# Escapes: " \ and control characters
#
# Usage: echo "$prompt" | escape_for_json
escape_for_json() {
    # Order matters: backslash must be first
    # Use awk for reliable newline handling across platforms
    awk '
    BEGIN { ORS="" }
    {
        gsub(/\\/, "\\\\")
        gsub(/"/, "\\\"")
        gsub(/\t/, "\\t")
        gsub(/\r/, "\\r")
        if (NR > 1) printf "\\n"
        printf "%s", $0
    }
    '
}

# Escape prompt for YAML string embedding
# For plain (unquoted) YAML strings, escape special characters
#
# Usage: echo "$prompt" | escape_for_yaml
escape_for_yaml() {
    # For YAML, it's safest to use quoted strings
    # But if we must escape, handle colons and quotes
    sed -e 's/\\/\\\\/g' \
        -e 's/"/\\"/g' \
        -e 's/:/\\:/g'
}

# Escape backticks for command substitution safety
# Prevents accidental command execution in backtick contexts
#
# Usage: echo "$prompt" | escape_backticks
escape_backticks() {
    sed 's/`/\\`/g'
}

# Escape dollar signs to prevent variable expansion
#
# Usage: echo "$prompt" | escape_dollar_signs
escape_dollar_signs() {
    sed 's/\$/\\$/g'
}

# Check if prompt contains potentially dangerous bash characters
# Returns 0 (true) if dangerous, 1 (false) if safe
#
# Usage:
#   echo "$prompt" | contains_dangerous_chars
#   contains_dangerous_chars "$prompt"
contains_dangerous_chars() {
    local input

    # Read from argument or stdin
    if [[ -n "$1" ]]; then
        input="$1"
    else
        input=$(cat)
    fi

    # Check for characters that could be dangerous in bash
    # Quotes, backticks, dollar signs, semicolons, pipes, etc.
    # Using [[ pattern matching which is more reliable
    if [[ "$input" == *["'\"\$\`\;\|\&\<\>\(\)\{\}\[\]"]* ]]; then
        return 0
    fi
    return 1
}

# Main escape_prompt function - dispatches to appropriate escaping method
#
# Usage: echo "$prompt" | escape_prompt [method]
# Methods:
#   heredoc        - No escaping (for quoted heredoc delimiters)
#   single_quotes  - Escape for single-quoted strings (default)
#   double_quotes  - Escape for double-quoted strings
#   bash_c         - Escape for bash -c command strings
#   json           - Escape for JSON strings
#   yaml           - Escape for YAML strings
#   raw            - No escaping (pass through)
#
# Example:
#   echo "Test's \"quote\" \`backtick\` \$var" | escape_prompt heredoc
#   echo "Test's quote" | escape_prompt single_quotes
escape_prompt() {
    local method="${1:-single_quotes}"

    case "$method" in
        heredoc|raw|none)
            escape_for_heredoc
            ;;
        single_quotes|single|sq)
            escape_for_single_quotes
            ;;
        double_quotes|double|dq)
            escape_for_double_quotes
            ;;
        bash_c|bash-c|bash)
            escape_for_bash_c
            ;;
        json)
            escape_for_json
            ;;
        yaml)
            escape_for_yaml
            ;;
        backticks|backtick)
            escape_backticks
            ;;
        dollar|dollars)
            escape_dollar_signs
            ;;
        *)
            # Unknown method, pass through with warning
            echo "Warning: Unknown escape method '$method', using pass-through" >&2
            cat
            ;;
    esac
}

# Get escape method name from alias
# Useful for normalizing user input
#
# Usage: get_escape_method "sq"
#   Returns: single_quotes
get_escape_method() {
    local alias="$1"

    case "$alias" in
        heredoc|raw|none)
            echo "heredoc"
            ;;
        single_quotes|single|sq)
            echo "single_quotes"
            ;;
        double_quotes|double|dq)
            echo "double_quotes"
            ;;
        bash_c|bash-c|bash)
            echo "bash_c"
            ;;
        json)
            echo "json"
            ;;
        yaml)
            echo "yaml"
            ;;
        *)
            echo "unknown"
            ;;
    esac
}

# Validate that escaped prompt is safe
# Returns 0 if safe, 1 if potentially unsafe
#
# Usage: validate_escaped_prompt "$escaped_prompt" "single_quotes"
validate_escaped_prompt() {
    local escaped="$1"
    local method="$2"

    case "$method" in
        single_quotes)
            # After escaping for single quotes, there should be no unescaped single quotes
            # Valid sequences: '\'' pattern
            if echo "$escaped" | grep -qE "(?<!\\\\)'(?!'\\\\'')"; then
                return 1
            fi
            ;;
        double_quotes)
            # After escaping for double quotes, check for unescaped special chars
            if echo "$escaped" | grep -qE '[^\\][$`"]'; then
                return 1
            fi
            ;;
    esac

    return 0
}
