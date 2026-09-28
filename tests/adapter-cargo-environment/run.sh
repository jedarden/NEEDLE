#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_root="$(mktemp -d "$repo_root/.adapter-cargo-environment.XXXXXX")"
trap 'rm -rf -- "$tmp_root"' EXIT

checker="$repo_root/scripts/check-adapter-cargo-environment.sh"

write_adapter() {
    local directory=$1 name=$2 content=$3
    mkdir -p "$directory"
    printf '%s\n' "$content" > "$directory/$name.yaml"
}

expect_pass() {
    local label=$1 directory=$2
    "$checker" --label "$label" --adapters-dir "$directory" >/dev/null
}

expect_failure() {
    local label=$1 directory=$2 expected=$3 output
    if output=$("$checker" --label "$label" --adapters-dir "$directory" 2>&1); then
        printf 'expected Cargo adapter policy to reject %s\n' "$label" >&2
        return 1
    fi
    if ! grep -Fq "$expected" <<<"$output"; then
        printf 'policy rejection for %s did not include %s:\n%s\n' \
            "$label" "$expected" "$output" >&2
        return 1
    fi
}

cases="$tmp_root/cases"
write_adapter "$cases/valid" valid \
    'invoke_template: "CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 agent"'
write_adapter "$cases/no-policy" no-policy \
    'invoke_template: "agent {prompt_file}"'
write_adapter "$cases/static-valid" static-valid \
    $'invoke_template: "agent {prompt_file}"\nenvironment:\n  CARGO_BUILD_JOBS: "2"\n  CARGO_INCREMENTAL: "0"\n  RUST_TEST_THREADS: "2"'
write_adapter "$cases/partial" partial \
    $'invoke_template: "agent {prompt_file}"\nenvironment:\n  CARGO_BUILD_JOBS: "2"'
write_adapter "$cases/wrong-value" wrong-value \
    $'invoke_template: "agent {prompt_file}"\nenvironment:\n  CARGO_BUILD_JOBS: "4"\n  CARGO_INCREMENTAL: "0"\n  RUST_TEST_THREADS: "2"'
write_adapter "$cases/rustflags-template" rustflags-template \
    'invoke_template: "RUSTFLAGS=\"-C codegen-units=1\" agent"'
write_adapter "$cases/rustflags-environment" rustflags-environment \
    $'invoke_template: "agent {prompt_file}"\nenvironment:\n  RUSTFLAGS: ""'
# Lab's real GLM adapters assign the Cargo policy inside a
# `systemd-run ... bash -c '...'` scope, where every inner single quote is
# the '\'' escape idiom. Before needle-01dea2ea the checker stopped at the
# escape's quote and read each value as a lone backslash, so valid adapters
# failed the gate. These cases pin the corrected unquoting.
mkdir -p "$cases/nested-idiom" "$cases/nested-idiom-bad"
cat > "$cases/nested-idiom/nested-idiom.yaml" <<'EOF'
invoke_template: systemd-run --user --scope --slice=needle.slice bash -c 'cd {workspace} && CARGO_BUILD_JOBS='\''2'\'' RUST_TEST_THREADS='\''2'\'' CARGO_INCREMENTAL='\''0'\'' agent < {prompt_file}'
environment: {}
EOF
cat > "$cases/nested-idiom-bad/nested-idiom-bad.yaml" <<'EOF'
invoke_template: systemd-run --user --scope --slice=needle.slice bash -c 'cd {workspace}; CARGO_BUILD_JOBS='\''4'\'' RUST_TEST_THREADS='\''2'\'' CARGO_INCREMENTAL='\''0'\'' RUSTFLAGS='\''-C codegen-units=1'\'' agent < {prompt_file}'
environment: {}
EOF

expect_pass valid "$cases/valid"
expect_pass no-policy "$cases/no-policy"
expect_pass static-valid "$cases/static-valid"
expect_failure partial "$cases/partial" 'missing CARGO_INCREMENTAL'
expect_failure wrong-value "$cases/wrong-value" 'CARGO_BUILD_JOBS=4'
expect_failure rustflags-template "$cases/rustflags-template" 'RUSTFLAGS is set'
expect_failure rustflags-environment "$cases/rustflags-environment" 'RUSTFLAGS is set'
expect_pass nested-idiom "$cases/nested-idiom"
expect_failure nested-idiom-bad "$cases/nested-idiom-bad" 'CARGO_BUILD_JOBS=4'
expect_failure nested-idiom-bad "$cases/nested-idiom-bad" 'RUSTFLAGS is set'

# Keep the documented checker self-test in this executable regression suite.
"$checker" --self-test >/dev/null

copy_apply_fixture() {
    local fleet=$1
    mkdir -p "$tmp_root/fleet" "$tmp_root/scripts"
    cp -a "$repo_root/fleet/$fleet" "$tmp_root/fleet/"
    cp "$checker" "$tmp_root/scripts/check-adapter-cargo-environment.sh"
}

adapter_snapshot() {
    local directory=$1
    find "$directory" -maxdepth 1 -type f -print0 | sort -z | xargs -0 -r sha256sum
}

test_ex44_apply_gate() {
    local home="$tmp_root/ex44-home" adapters adapter_target before after output
    copy_apply_fixture ex44
    adapters="$home/.config/needle/adapters"
    mkdir -p "$adapters" "$home/.config/systemd/user"

    adapter_target=$(find "$tmp_root/fleet/ex44/adapters" -maxdepth 1 -type f -name '*.yaml' -print -quit)
    [[ -n "$adapter_target" ]] || { echo 'ex44 fixture has no tracked adapters' >&2; return 1; }
    adapter_target="$adapters/$(basename "$adapter_target")"
    write_adapter "$adapters" "$(basename "$adapter_target" .yaml)" \
        'invoke_template: "agent {prompt_file}"'
    write_adapter "$adapters" invalid \
        'invoke_template: "RUSTFLAGS=\"-C codegen-units=1\" agent"'
    before=$(adapter_snapshot "$adapters")

    if output=$(NEEDLE_HOST_HOME="$home" HOME="$home" \
        "$tmp_root/fleet/ex44/apply-ex44-fleet.sh" 2>&1); then
        echo 'ex44 apply unexpectedly accepted a forbidden RUSTFLAGS adapter' >&2
        return 1
    fi
    grep -Fq 'RUSTFLAGS is set' <<<"$output"
    after=$(adapter_snapshot "$adapters")
    [[ "$after" == "$before" ]] || {
        echo 'ex44 apply changed host adapters before rejecting policy' >&2
        return 1
    }
    [[ ! -e "$home/.config/systemd/user/needle.slice" ]] || {
        echo 'ex44 apply modified fleet files before rejecting policy' >&2
        return 1
    }
}

test_lab_apply_gate() {
    local home="$tmp_root/lab-home" adapters before after output
    copy_apply_fixture lab
    adapters="$home/.config/needle/adapters"
    mkdir -p "$adapters" "$home/.config/needle/workers" "$home/.config/systemd/user"
    write_adapter "$adapters" valid 'invoke_template: "agent {prompt_file}"'
    write_adapter "$adapters" invalid \
        $'invoke_template: "agent {prompt_file}"\nenvironment:\n  CARGO_BUILD_JOBS: "2"'
    before=$(adapter_snapshot "$adapters")

    if output=$(HOME="$home" "$tmp_root/fleet/lab/apply-lab-fleet.sh" 2>&1); then
        echo 'lab apply unexpectedly accepted a partial Cargo policy' >&2
        return 1
    fi
    grep -Fq 'missing CARGO_INCREMENTAL' <<<"$output"
    after=$(adapter_snapshot "$adapters")
    [[ "$after" == "$before" ]] || {
        echo 'lab apply changed host adapters before rejecting policy' >&2
        return 1
    }
    [[ ! -e "$home/.config/systemd/user/needle.slice" ]] || {
        echo 'lab apply modified fleet files before rejecting policy' >&2
        return 1
    }
    [[ -z "$(find "$home/.config/needle/workers" -mindepth 1 -print -quit)" ]] || {
        echo 'lab apply modified worker configuration before rejecting policy' >&2
        return 1
    }
}

test_ex44_apply_gate
test_lab_apply_gate

echo 'adapter cargo environment: PASS'
