#!/usr/bin/env bash
# Drain complete NEEDLE attempt-archive spool pairs to an S3-compatible sink.
#
# NEEDLE itself never invokes this script.  It is intentionally an external
# process so that the only sink credential lives in the rclone configuration
# owned by the operator.

set -Eeuo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: drain.sh --spool-dir DIR --rclone-config FILE --remote NAME --bucket NAME [--allow-insecure]

Drain complete attempt-archive bundle/sidecar pairs to:
  NAME:BUCKET/transcripts/HOST/WORKSPACE/BEAD/ATTEMPT/FILE

The rclone configuration must be readable only by its owner. Explicit HTTP
endpoints require --allow-insecure (intended for local test servers only).
EOF
}

die() {
  printf 'attempt archive drain: %s\n' "$*" >&2
  exit 1
}

spool_dir=''
rclone_config=''
remote=''
bucket=''
allow_insecure=false

while (($# > 0)); do
  case "$1" in
    --spool-dir)
      (($# >= 2)) || die "--spool-dir requires a directory"
      spool_dir=$2
      shift 2
      ;;
    --rclone-config)
      (($# >= 2)) || die "--rclone-config requires a file"
      rclone_config=$2
      shift 2
      ;;
    --remote)
      (($# >= 2)) || die "--remote requires a name"
      remote=$2
      shift 2
      ;;
    --bucket)
      (($# >= 2)) || die "--bucket requires a name"
      bucket=$2
      shift 2
      ;;
    --allow-insecure)
      allow_insecure=true
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      usage
      die "unknown argument: $1"
      ;;
  esac
done

[[ -n "$spool_dir" ]] || { usage; die "--spool-dir is required"; }
[[ -n "$rclone_config" ]] || { usage; die "--rclone-config is required"; }
[[ -n "$remote" ]] || { usage; die "--remote is required"; }
[[ -n "$bucket" ]] || { usage; die "--bucket is required"; }

command -v rclone >/dev/null 2>&1 || die "rclone is not installed"
command -v jq >/dev/null 2>&1 || die "jq is not installed"
command -v sha256sum >/dev/null 2>&1 || die "sha256sum is not installed"

[[ -f "$rclone_config" ]] || die "rclone config does not exist: $rclone_config"
[[ -r "$rclone_config" ]] || die "rclone config is not readable: $rclone_config"

# A credential-bearing rclone config must not be group- or world-readable.
config_mode=$(stat -c '%a' "$rclone_config" 2>/dev/null) \
  || die "cannot inspect rclone config permissions: $rclone_config"
config_mode=$((8#$config_mode))
(( (config_mode & 77) == 0 )) \
  || die "rclone config must not be group- or world-readable: $rclone_config"

[[ "$remote" =~ ^[A-Za-z0-9._-]+$ ]] \
  || die "rclone remote must be a simple remote name"
[[ "$bucket" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] \
  || die "bucket must be a simple S3 bucket name"

mkdir -p "$spool_dir" || die "cannot create spool directory: $spool_dir"
spool_dir=$(cd "$spool_dir" && pwd -P) \
  || die "cannot resolve spool directory: $spool_dir"

# Read only the endpoint field from rclone's JSON configuration. The complete
# dump is kept in a shell variable and is never printed, because it can also
# contain the sink credential. An absent endpoint means the provider default
# (AWS and similar providers use HTTPS); an endpoint that cannot be proven to
# be HTTPS is rejected unless the operator explicitly opts in.
config_dump=$(rclone config dump --config "$rclone_config" 2>/dev/null) \
  || die "rclone could not read the configured remote"
jq -e --arg remote "$remote" 'has($remote)' <<<"$config_dump" >/dev/null 2>&1 \
  || die "rclone remote is not present in the configured file: $remote"
endpoint=$(jq -er --arg remote "$remote" '.[$remote].endpoint // empty' <<<"$config_dump" 2>/dev/null) \
  || endpoint=''
case "$endpoint" in
  '')
    ;;
  https://*)
    ;;
  http://*)
    [[ "$allow_insecure" == true ]] \
      || die "rclone endpoint is not TLS; pass --allow-insecure only for a trusted local test endpoint"
    ;;
  *)
    [[ "$allow_insecure" == true ]] \
      || die "rclone endpoint is not explicitly TLS; pass --allow-insecure only for a trusted local test endpoint"
    ;;
esac

RCLONE=(
  rclone
  --config "$rclone_config"
  --quiet
  --retries "${ATTEMPT_ARCHIVE_RCLONE_RETRIES:-3}"
  --retries-sleep "${ATTEMPT_ARCHIVE_RCLONE_RETRY_SLEEP:-5s}"
  --low-level-retries "${ATTEMPT_ARCHIVE_RCLONE_LOW_LEVEL_RETRIES:-10}"
)

uploaded=0
failed=0
bytes=0

write_last_drain() {
  local finished_at tmp
  finished_at=$(date -u '+%Y-%m-%dT%H:%M:%SZ') \
    || return 1
  tmp="$spool_dir/.last-drain.json.partial.$$"
  jq -n \
    --arg finished_at "$finished_at" \
    --argjson uploaded "$uploaded" \
    --argjson failed "$failed" \
    --argjson bytes "$bytes" \
    '{finished_at: $finished_at, uploaded: $uploaded, failed: $failed, bytes: $bytes}' \
    >"$tmp" \
    && chmod 600 "$tmp" \
    && mv -f -- "$tmp" "$spool_dir/last-drain.json"
}

remote_size() {
  local listing
  listing=$(
    "${RCLONE[@]}" lsjson --files-only --no-modtime --max-depth 1 "$1" 2>/dev/null
  ) || return 1
  jq -er 'if (type == "array" and length == 1 and (.[0].Size | numbers)) then .[0].Size else error end' \
    <<<"$listing" 2>/dev/null
}

ensure_remote_file() {
  local source=$1 destination=$2 expected_size=$3 existing_size

  # A correctly sized object is the idempotent success path. A missing object
  # and a wrong-sized object both go through copyto; the subsequent size check
  # is the durability/verification boundary for this drain.
  existing_size=$(remote_size "$destination" || true)
  if [[ "$existing_size" == "$expected_size" ]]; then
    return 0
  fi

  "${RCLONE[@]}" copyto "$source" "$destination" \
    || return 1
  existing_size=$(remote_size "$destination") \
    || return 1
  [[ "$existing_size" == "$expected_size" ]]
}

sidecar_string() {
  jq -er --arg field "$1" \
    '.[$field] | select(type == "string" and length > 0)' "$2"
}

safe_component() {
  [[ "$1" =~ ^[A-Za-z0-9._~-]+$ && "$1" != '.' && "$1" != '..' ]]
}

process_pair() {
  local bundle=$1 sidecar suffix stem relative bundle_path
  local bundle_bytes bundle_sha256 actual_bytes actual_sha256 sidecar_bytes
  local host workspace_slug bead_id attempt_id key_prefix bundle_key sidecar_key component

  if [[ "$bundle" == *.tar.zst ]]; then
    suffix='.tar.zst'
  elif [[ "$bundle" == *.tar ]]; then
    suffix='.tar'
  else
    return 0
  fi
  stem=${bundle%"$suffix"}
  sidecar="${stem}.json"
  [[ -f "$sidecar" ]] || return 0

  if ! bundle_bytes=$(jq -er \
    '.bundle_bytes | numbers | select(. >= 0 and . == floor) | tostring' "$sidecar" 2>/dev/null); then
    printf 'attempt archive drain: invalid bundle_bytes in %s\n' "$sidecar" >&2
    return 1
  fi
  if ! bundle_sha256=$(jq -er \
    '.bundle_sha256 | strings | select(test("^[0-9a-fA-F]{64}$")) | ascii_downcase' "$sidecar" 2>/dev/null); then
    printf 'attempt archive drain: invalid bundle_sha256 in %s\n' "$sidecar" >&2
    return 1
  fi
  if ! bundle_path=$(sidecar_string bundle_path "$sidecar" 2>/dev/null); then
    printf 'attempt archive drain: missing bundle_path in %s\n' "$sidecar" >&2
    return 1
  fi
  relative=${bundle#"$spool_dir"/}
  [[ "$bundle_path" == "$relative" ]] || {
    printf 'attempt archive drain: bundle_path mismatch in %s\n' "$sidecar" >&2
    return 1
  }

  actual_bytes=$(stat -c '%s' "$bundle") \
    || { printf 'attempt archive drain: cannot stat %s\n' "$bundle" >&2; return 1; }
  actual_sha256=$(sha256sum "$bundle" | awk '{print $1}') \
    || { printf 'attempt archive drain: cannot hash %s\n' "$bundle" >&2; return 1; }
  if [[ "$actual_bytes" != "$bundle_bytes" || "$actual_sha256" != "$bundle_sha256" ]]; then
    printf 'attempt archive drain: bundle verification failed for %s\n' "$bundle" >&2
    return 1
  fi

  host=$(sidecar_string host "$sidecar") \
    || { printf 'attempt archive drain: missing host in %s\n' "$sidecar" >&2; return 1; }
  workspace_slug=$(sidecar_string workspace_slug "$sidecar") \
    || { printf 'attempt archive drain: missing workspace_slug in %s\n' "$sidecar" >&2; return 1; }
  bead_id=$(sidecar_string bead_id "$sidecar") \
    || { printf 'attempt archive drain: missing bead_id in %s\n' "$sidecar" >&2; return 1; }
  attempt_id=$(sidecar_string attempt_id "$sidecar") \
    || { printf 'attempt archive drain: missing attempt_id in %s\n' "$sidecar" >&2; return 1; }
  for component in "$host" "$workspace_slug" "$bead_id" "$attempt_id"; do
    safe_component "$component" || {
      printf 'attempt archive drain: unsafe object-key component in %s\n' "$sidecar" >&2
      return 1
    }
  done
  [[ "$attempt_id" == "${stem##*/}" ]] || {
    printf 'attempt archive drain: attempt_id does not match filename in %s\n' "$sidecar" >&2
    return 1
  }

  sidecar_bytes=$(stat -c '%s' "$sidecar") \
    || { printf 'attempt archive drain: cannot stat %s\n' "$sidecar" >&2; return 1; }
  key_prefix="${remote}:${bucket}/transcripts/${host}/${workspace_slug}/${bead_id}"
  bundle_key="$key_prefix/${attempt_id}${suffix}"
  sidecar_key="$key_prefix/${attempt_id}.json"

  if ! ensure_remote_file "$bundle" "$bundle_key" "$bundle_bytes"; then
    printf 'attempt archive drain: bundle upload or remote verification failed for %s\n' "$bundle" >&2
    return 1
  fi
  if ! ensure_remote_file "$sidecar" "$sidecar_key" "$sidecar_bytes"; then
    printf 'attempt archive drain: sidecar upload or remote verification failed for %s\n' "$sidecar" >&2
    return 1
  fi

  rm -f -- "$bundle" "$sidecar" \
    || { printf 'attempt archive drain: could not remove local pair %s\n' "$bundle" >&2; return 1; }
  uploaded=$((uploaded + 1))
  bytes=$((bytes + bundle_bytes))
  return 0
}

while IFS= read -r -d '' bundle; do
  if ! process_pair "$bundle"; then
    failed=$((failed + 1))
  fi
done < <(find "$spool_dir" -type f \( -name '*.tar.zst' -o -name '*.tar' \) -print0)

write_last_drain \
  || { printf 'attempt archive drain: could not write %s/last-drain.json\n' "$spool_dir" >&2; exit 1; }

if (( failed > 0 )); then
  exit 1
fi
