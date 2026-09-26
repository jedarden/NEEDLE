#!/usr/bin/env bash
# Validate the Cargo environment contract used by fleet agent adapters.
#
# An adapter may omit the Cargo block when it does not use the fleet build
# policy. If it declares any of the three bounded-build variables, however,
# it must declare all three with their exact values. RUSTFLAGS is forbidden in
# both invoke_template assignments and the static environment map.
set -euo pipefail

EXPECTED_CARGO_BUILD_JOBS=2
EXPECTED_CARGO_INCREMENTAL=0
EXPECTED_RUST_TEST_THREADS=2

usage() {
    cat >&2 <<'EOF'
usage: check-adapter-cargo-environment.sh [--self-test]
       [--label LABEL --adapters-dir DIRECTORY]...

Validate fleet adapter Cargo environment assignments. Repeat the label and
directory pair to check multiple adapter counterparts in one invocation.
EOF
}

policy_records() {
    # This is intentionally a narrow parser for the adapter schema. Policy
    # values can appear in the shell command template or in its static
    # environment map; unrelated YAML descriptions and comments are ignored.
    awk -f - "$1" <<'AWK'
        function leading_spaces(line) {
            match(line, /^[[:space:]]*/)
            return RLENGTH
        }

        function yaml_value(value, quote) {
            sub(/[[:space:]]+#.*$/, "", value)
            gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
            if (substr(value, 1, 1) == "\"" && substr(value, length(value), 1) == "\"") {
                value = substr(value, 2, length(value) - 2)
                gsub(/\\"/, "\"", value)
            } else if (substr(value, 1, 1) == "'" && substr(value, length(value), 1) == "'") {
                value = substr(value, 2, length(value) - 2)
            }
            return value
        }

        function escaped_shell_quotes(value) {
            if (length(value) >= 4 && substr(value, 1, 2) == "\\\"" && substr(value, length(value) - 1, 2) == "\\\"") {
                return substr(value, 3, length(value) - 4)
            }
            if (length(value) >= 4 && substr(value, 1, 2) == "\\'" && substr(value, length(value) - 1, 2) == "\\'") {
                return substr(value, 3, length(value) - 4)
            }
            return value
        }

        function shell_value(rest, quote, closing, separator_pos) {
            if (substr(rest, 1, 1) == "\"" || substr(rest, 1, 1) == "'") {
                quote = substr(rest, 1, 1)
                rest = substr(rest, 2)
                closing = index(rest, quote)
                if (closing) {
                    return escaped_shell_quotes(substr(rest, 1, closing - 1))
                }
                return escaped_shell_quotes(rest)
            }
            separator_pos = match(rest, /[[:space:];|&]/)
            if (separator_pos) {
                return escaped_shell_quotes(substr(rest, 1, RSTART - 1))
            }
            return escaped_shell_quotes(rest)
        }

        function emit_assignment(line, variable, pattern, rest) {
            pattern = "(^|[;[:space:]\"'])" variable "[[:space:]]*=[[:space:]]*"
            if (match(line, pattern)) {
                rest = substr(line, RSTART + RLENGTH)
                print variable "\t" shell_value(rest) "\tinvoke_template"
            }
        }

        function emit_map_entry(line, variable, value) {
            if (line !~ /^[[:space:]]*(CARGO_BUILD_JOBS|CARGO_INCREMENTAL|RUST_TEST_THREADS|RUSTFLAGS):[[:space:]]*/) {
                return
            }
            variable = line
            sub(/^[[:space:]]*/, "", variable)
            sub(/:.*/, "", variable)
            value = line
            sub(/^[[:space:]]*[^:]+:[[:space:]]*/, "", value)
            print variable "\t" yaml_value(value) "\tenvironment"
        }

        {
            line = $0
            indent = leading_spaces(line)

            if (in_invoke) {
                # A less-indented YAML key ends a block scalar. A quoted
                # one-line value is harmless here: the next key closes it.
                if (NR != invoke_line && line ~ /^[[:space:]]*[A-Za-z0-9_-]+:[[:space:]]*/ && indent <= invoke_indent) {
                    in_invoke = 0
                } else {
                    emit_assignment(line, "CARGO_BUILD_JOBS")
                    emit_assignment(line, "CARGO_INCREMENTAL")
                    emit_assignment(line, "RUST_TEST_THREADS")
                    emit_assignment(line, "RUSTFLAGS")
                    next
                }
            }

            if (in_environment) {
                if (NR != environment_line && line ~ /^[[:space:]]*[A-Za-z0-9_-]+:[[:space:]]*/ && indent <= environment_indent) {
                    in_environment = 0
                } else if (indent > environment_indent) {
                    emit_map_entry(line)
                    next
                }
            }

            if (line ~ /^[[:space:]]*invoke_template:[[:space:]]*/) {
                in_invoke = 1
                invoke_line = NR
                invoke_indent = indent
                emit_assignment(line, "CARGO_BUILD_JOBS")
                emit_assignment(line, "CARGO_INCREMENTAL")
                emit_assignment(line, "RUST_TEST_THREADS")
                emit_assignment(line, "RUSTFLAGS")
                next
            }

            if (line ~ /^[[:space:]]*environment:[[:space:]]*/) {
                in_environment = 1
                environment_line = NR
                environment_indent = indent
                next
            }
        }
AWK
}

check_directory() {
    local label=$1 directory=$2 file basename_file variable expected_var expected
    local violations=0 checked=0
    local -a files=()

    if [[ ! -d "$directory" ]]; then
        printf '%s: adapter directory does not exist: %s\n' "$label" "$directory" >&2
        return 1
    fi

    mapfile -t files < <(find "$directory" -maxdepth 1 -type f \( -name '*.yaml' -o -name '*.yml' \) -print | sort)
    if [[ "${#files[@]}" -eq 0 ]]; then
        printf '%s: adapter directory contains no YAML adapters: %s\n' "$label" "$directory" >&2
        return 1
    fi

    for file in "${files[@]}"; do
        basename_file=$(basename "$file")
        checked=$((checked + 1))
        declare -A counts=(
            [CARGO_BUILD_JOBS]=0
            [CARGO_INCREMENTAL]=0
            [RUST_TEST_THREADS]=0
            [RUSTFLAGS]=0
        )
        while IFS=$'\t' read -r variable value source; do
            [[ -n "$variable" ]] || continue
            counts[$variable]=$((counts[$variable] + 1))
            if [[ "$variable" == RUSTFLAGS ]]; then
                printf '%s/%s: RUSTFLAGS is set in %s; it must be unset\n' \
                    "$label" "$basename_file" "$source" >&2
                violations=$((violations + 1))
            else
                expected_var=EXPECTED_$variable
                expected=${!expected_var}
                if [[ "$value" != "$expected" ]]; then
                    printf '%s/%s: %s=%s in %s (expected %s)\n' \
                        "$label" "$basename_file" "$variable" \
                        "${value:-<empty>}" "$source" "$expected" >&2
                    violations=$((violations + 1))
                fi
            fi
        done < <(policy_records "$file")

        if (( counts[CARGO_BUILD_JOBS] > 0 || counts[CARGO_INCREMENTAL] > 0 || counts[RUST_TEST_THREADS] > 0 )); then
            for variable in CARGO_BUILD_JOBS CARGO_INCREMENTAL RUST_TEST_THREADS; do
                if (( counts[$variable] == 0 )); then
                    printf '%s/%s: missing %s from partial Cargo policy block\n' \
                        "$label" "$basename_file" "$variable" >&2
                    violations=$((violations + 1))
                    continue
                fi
            done
        fi
    done

    if (( violations > 0 )); then
        printf '%s: %d adapter cargo-environment violation(s) across %d adapter(s)\n' \
            "$label" "$violations" "$checked" >&2
        return 1
    fi

    printf '%s: checked %d adapter(s); cargo environment policy passed\n' "$label" "$checked"
}

self_test() {
    local tmp output
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/needle-adapter-policy.XXXXXX")
    trap 'rm -rf -- "${tmp:-}"' EXIT
    mkdir -p "$tmp/codinghome" "$tmp/lab" "$tmp/bad"

    printf '%s\n' \
        'name: valid' \
        'invoke_template: "CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS='"'"'2'"'"' agent"' \
        'environment: {}' > "$tmp/codinghome/valid.yaml"
    printf '%s\n' \
        'name: no-policy-block' \
        'invoke_template: "agent {prompt_file}"' \
        'environment: {}' > "$tmp/lab/no-policy-block.yaml"
    printf '%s\n' \
        'name: static-policy-block' \
        'invoke_template: "agent {prompt_file}"' \
        'environment:' \
        '  CARGO_BUILD_JOBS: "2"' \
        '  CARGO_INCREMENTAL: "0"' \
        '  RUST_TEST_THREADS: "2"' > "$tmp/lab/static-policy-block.yaml"
    printf '%s\n' \
        'name: invalid' \
        'invoke_template: |' \
        '  CARGO_BUILD_JOBS=4' \
        '  CARGO_INCREMENTAL=0' \
        '  RUSTFLAGS="-C codegen-units=1"' > "$tmp/bad/invalid.yaml"
    printf '%s\n' \
        'name: invalid-static' \
        'invoke_template: "agent {prompt_file}"' \
        'environment:' \
        '  RUSTFLAGS: ""' > "$tmp/bad/invalid-static.yaml"

    "$0" --label codinghome --adapters-dir "$tmp/codinghome" \
        --label lab --adapters-dir "$tmp/lab" >/dev/null
    if output=$("$0" --label bad --adapters-dir "$tmp/bad" 2>&1); then
        printf 'self-test expected an invalid adapter to fail\n' >&2
        return 1
    fi
    grep -q 'bad/invalid.yaml: CARGO_BUILD_JOBS=4' <<<"$output"
    grep -q 'bad/invalid.yaml: missing RUST_TEST_THREADS' <<<"$output"
    grep -q 'bad/invalid.yaml: RUSTFLAGS is set' <<<"$output"
    grep -q 'bad/invalid-static.yaml: RUSTFLAGS is set' <<<"$output"
    printf 'adapter cargo-environment policy self-test passed\n'
}

main() {
    local self_test_mode=0 current_label=adapter-policy arg
    local -a checks=()

    while [[ "$#" -gt 0 ]]; do
        arg=$1
        case "$arg" in
            --self-test)
                self_test_mode=1
                shift
                ;;
            --label)
                [[ "$#" -ge 2 ]] || { usage; return 2; }
                current_label=$2
                shift 2
                ;;
            --adapters-dir)
                [[ "$#" -ge 2 ]] || { usage; return 2; }
                checks+=("$current_label"$'\t'"$2")
                current_label=adapter-policy
                shift 2
                ;;
            -h|--help)
                usage 2>&1
                return 0
                ;;
            *)
                printf 'unknown argument: %s\n' "$arg" >&2
                usage
                return 2
                ;;
        esac
    done

    if (( self_test_mode == 1 )); then
        self_test
        return
    fi
    if [[ "${#checks[@]}" -eq 0 ]]; then
        usage
        return 2
    fi

    local spec label directory status=0
    for spec in "${checks[@]}"; do
        IFS=$'\t' read -r label directory <<<"$spec"
        if ! check_directory "$label" "$directory"; then
            status=1
        fi
    done
    return "$status"
}

main "$@"
