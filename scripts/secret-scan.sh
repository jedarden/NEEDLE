#!/usr/bin/env bash
# Run repository secret verification with NEEDLE's vendored ruleset.

set -uo pipefail

die() {
    printf 'secret-scan: error: %s\n' "$*" >&2
    exit 2
}

usage() {
    cat >&2 <<'EOF'
Usage:
  scripts/secret-scan.sh staged
  scripts/secret-scan.sh directory PATH

The scanner must meet config/gitleaks.toml's minVersion. Findings are always
redacted, and every run identifies the scanner version and config digest.
EOF
    exit 2
}

parse_semver() {
    local input="$1"
    if [[ "$input" =~ ([0-9]+)\.([0-9]+)\.([0-9]+) ]]; then
        printf '%s.%s.%s\n' \
            "$((10#${BASH_REMATCH[1]}))" \
            "$((10#${BASH_REMATCH[2]}))" \
            "$((10#${BASH_REMATCH[3]}))"
        return 0
    fi
    return 1
}

version_at_least() {
    local actual="$1"
    local minimum="$2"
    local actual_major actual_minor actual_patch
    local minimum_major minimum_minor minimum_patch

    IFS=. read -r actual_major actual_minor actual_patch <<<"$actual"
    IFS=. read -r minimum_major minimum_minor minimum_patch <<<"$minimum"

    ((actual_major > minimum_major)) && return 0
    ((actual_major < minimum_major)) && return 1
    ((actual_minor > minimum_minor)) && return 0
    ((actual_minor < minimum_minor)) && return 1
    ((actual_patch >= minimum_patch))
}

[[ $# -ge 1 ]] || usage

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source_root="$(cd "$script_dir/.." && pwd)" \
    || die 'cannot resolve the NEEDLE source root'
config_path="$source_root/config/gitleaks.toml"
[[ -f "$config_path" ]] || die "missing vendored config: $config_path"

minimum_raw="$(sed -nE 's/^[[:space:]]*minVersion[[:space:]]*=[[:space:]]*"v?([0-9]+\.[0-9]+\.[0-9]+)".*/\1/p' "$config_path" | head -n 1)"
minimum_version="$(parse_semver "$minimum_raw")" \
    || die 'vendored config has no supported semantic minVersion'

gitleaks_bin=''
scanner_version=''
select_supported_scanner() {
    local candidate="$1"
    local scanner_output candidate_version

    [[ -x "$candidate" ]] || return 1
    scanner_output="$("$candidate" version 2>&1)" || return 1
    candidate_version="$(parse_semver "$scanner_output")" || return 1
    version_at_least "$candidate_version" "$minimum_version" || return 1

    gitleaks_bin="$candidate"
    scanner_version="$candidate_version"
}

if [[ -n "${GITLEAKS_BIN:-}" ]]; then
    gitleaks_bin="$GITLEAKS_BIN"
    if [[ "$gitleaks_bin" != */* ]]; then
        gitleaks_bin="$(command -v "$gitleaks_bin" 2>/dev/null)" \
            || die "scanner is not available: $GITLEAKS_BIN"
    fi
    [[ -x "$gitleaks_bin" ]] || die "scanner is not executable: $gitleaks_bin"
    scanner_output="$("$gitleaks_bin" version 2>&1)" \
        || die 'scanner version probe failed'
    scanner_version="$(parse_semver "$scanner_output")" \
        || die 'scanner returned an unrecognized version'
    version_at_least "$scanner_version" "$minimum_version" \
        || die "gitleaks $scanner_version is older than required $minimum_version"
else
    declare -A seen_candidates=()
    while IFS= read -r candidate; do
        [[ -n "$candidate" && -z "${seen_candidates[$candidate]:-}" ]] || continue
        seen_candidates["$candidate"]=1
        if select_supported_scanner "$candidate"; then
            break
        fi
    done < <(type -a -p gitleaks 2>/dev/null || true)
    [[ -n "$gitleaks_bin" ]] \
        || die "no gitleaks on PATH meets required version $minimum_version"
fi

config_sha256="$(sha256sum "$config_path" | awk '{print $1}')" \
    || die 'could not hash the vendored config'
scanner_sha256="$(sha256sum "$gitleaks_bin" | awk '{print $1}')" \
    || die 'could not hash the scanner executable'
mode="$1"
shift

case "$mode" in
    staged)
        [[ $# -eq 0 ]] || usage
        repo_root="$(git -C "$source_root" rev-parse --show-toplevel 2>/dev/null)" \
            || die 'staged mode requires a Git worktree'
        scan_args=(git --staged --redact --no-banner --config "$config_path" "$repo_root")
        ;;
    directory)
        [[ $# -eq 1 ]] || usage
        scan_target="$1"
        [[ -d "$scan_target" ]] || die "scan target is not a directory: $scan_target"
        scan_args=(dir --redact --no-banner --config "$config_path" "$scan_target")
        ;;
    *)
        usage
        ;;
esac

printf 'secret-scan: mode=%s scanner=gitleaks version=%s scanner_sha256=%s config_sha256=%s\n' \
    "$mode" "$scanner_version" "$scanner_sha256" "$config_sha256" >&2

"$gitleaks_bin" "${scan_args[@]}"
scan_status=$?
printf 'secret-scan: result=%s\n' "$scan_status" >&2
exit "$scan_status"
