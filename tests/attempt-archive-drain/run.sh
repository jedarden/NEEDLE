#!/usr/bin/env bash
# End-to-end coverage for the external attempt-archive drain.
#
# The server is an rclone S3 gateway backed by a temporary local directory, so
# this test exercises the same S3 API and remote-size checks as a real sink
# without needing a network service or credentials.

set -Eeuo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DRAIN="$REPO_ROOT/contrib/attempt-archive/drain.sh"
[[ -x "$DRAIN" ]] || { echo "missing executable drain: $DRAIN" >&2; exit 1; }

command -v rclone >/dev/null 2>&1 || { echo "rclone is required" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 1; }
command -v sha256sum >/dev/null 2>&1 || { echo "sha256sum is required" >&2; exit 1; }

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/needle-attempt-archive.XXXXXX")"
SERVER_PID=''

cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf -- "$TMP_DIR"
}
trap cleanup EXIT

SPOOL="$TMP_DIR/spool"
BACKEND="$TMP_DIR/backend"
RCLONE_CONFIG="$TMP_DIR/rclone.conf"
mkdir -p "$SPOOL" "$BACKEND"

PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
AUTH_KEY='test-access,test-secret'
rclone serve s3 --addr "127.0.0.1:$PORT" --auth-key "$AUTH_KEY" "$BACKEND" \
  >"$TMP_DIR/server.log" 2>&1 &
SERVER_PID=$!

server_ready=false
for _ in $(seq 1 50); do
  if (echo >/dev/tcp/127.0.0.1/"$PORT") 2>/dev/null; then
    server_ready=true
    break
  fi
  sleep 0.1
done
[[ "$server_ready" == true ]] || {
  sed -n '1,80p' "$TMP_DIR/server.log" >&2
  echo "rclone serve s3 did not start" >&2
  exit 1
}

cat >"$RCLONE_CONFIG" <<EOF
[sink]
type = s3
provider = Rclone
endpoint = http://127.0.0.1:$PORT
access_key_id = test-access
secret_access_key = test-secret
EOF
chmod 600 "$RCLONE_CONFIG"

assert_eq() {
  local actual=$1 expected=$2 description=$3
  if [[ "$actual" != "$expected" ]]; then
    printf 'FAIL: %s (expected %s, got %s)\n' "$description" "$expected" "$actual" >&2
    exit 1
  fi
}

assert_file() {
  local path=$1 description=$2
  [[ -f "$path" ]] || { printf 'FAIL: missing %s (%s)\n' "$path" "$description" >&2; exit 1; }
}

assert_absent() {
  local path=$1 description=$2
  [[ ! -e "$path" ]] || { printf 'FAIL: present %s (%s)\n' "$path" "$description" >&2; exit 1; }
}

make_pair() {
  local attempt_id=$1
  local dir="$SPOOL/host-a/workspace-a/bead-123"
  local bundle="$dir/$attempt_id.tar.zst"
  local sidecar="$dir/$attempt_id.json"
  local bundle_bytes bundle_sha256

  mkdir -p "$dir"
  printf 'bundle for %s\n' "$attempt_id" >"$bundle"
  bundle_bytes="$(stat -c '%s' "$bundle")"
  bundle_sha256="$(sha256sum "$bundle" | awk '{print $1}')"
  jq -n \
    --arg bundle_path "host-a/workspace-a/bead-123/$attempt_id.tar.zst" \
    --arg bundle_sha256 "$bundle_sha256" \
    --argjson bundle_bytes "$bundle_bytes" \
    --arg attempt_id "$attempt_id" \
    '{schema_version: 1, bundle_path: $bundle_path, bundle_sha256: $bundle_sha256,
      bundle_bytes: $bundle_bytes, host: "host-a", workspace: "/work/workspace-a",
      workspace_slug: "workspace-a", bead_id: "bead-123", attempt_id: $attempt_id,
      provisional: false, session_id: null, adapter: "test", model: null,
      worker_id: "test-worker", outcome: "success", requested_action: null,
      started_at: null, finished_at: null, contents: ["prompt.md"]}' \
    >"$sidecar"
}

run_drain() {
  "$DRAIN" \
    --spool-dir "$SPOOL" \
    --rclone-config "$RCLONE_CONFIG" \
    --remote sink \
    --bucket archive \
    --allow-insecure
}

make_pair attempt-1
cp "$SPOOL/host-a/workspace-a/bead-123/attempt-1.tar.zst" "$TMP_DIR/attempt-1.tar.zst"
cp "$SPOOL/host-a/workspace-a/bead-123/attempt-1.json" "$TMP_DIR/attempt-1.json"

printf 'orphan bundle\n' >"$SPOOL/host-a/workspace-a/bead-123/orphan.tar.zst"
printf 'partial bundle\n' >"$SPOOL/host-a/workspace-a/bead-123/partial.tar.zst.partial"

run_drain
assert_absent "$SPOOL/host-a/workspace-a/bead-123/attempt-1.tar.zst" 'uploaded bundle removed'
assert_absent "$SPOOL/host-a/workspace-a/bead-123/attempt-1.json" 'uploaded sidecar removed'
assert_file "$SPOOL/host-a/workspace-a/bead-123/orphan.tar.zst" 'bundle without sidecar retained'
assert_file "$SPOOL/host-a/workspace-a/bead-123/partial.tar.zst.partial" '.partial bundle retained'

REMOTE_DIR="$BACKEND/archive/transcripts/host-a/workspace-a/bead-123"
assert_file "$REMOTE_DIR/attempt-1.tar.zst" 'bundle exists at the required object key'
assert_file "$REMOTE_DIR/attempt-1.json" 'sidecar exists at the required object key'
cmp "$TMP_DIR/attempt-1.tar.zst" "$REMOTE_DIR/attempt-1.tar.zst"
cmp "$TMP_DIR/attempt-1.json" "$REMOTE_DIR/attempt-1.json"
assert_eq "$(jq -r '.uploaded' "$SPOOL/last-drain.json")" 1 'first drain uploaded count'
assert_eq "$(jq -r '.failed' "$SPOOL/last-drain.json")" 0 'first drain failed count'
assert_eq "$(jq -r '.bytes' "$SPOOL/last-drain.json")" "$(stat -c '%s' "$TMP_DIR/attempt-1.tar.zst")" 'first drain byte count'

# Recreate a complete local pair after the remote objects exist. The drain
# must verify those objects, skip uploads, and still remove this local copy.
cp "$TMP_DIR/attempt-1.tar.zst" "$SPOOL/host-a/workspace-a/bead-123/attempt-1.tar.zst"
cp "$TMP_DIR/attempt-1.json" "$SPOOL/host-a/workspace-a/bead-123/attempt-1.json"
run_drain
assert_absent "$SPOOL/host-a/workspace-a/bead-123/attempt-1.tar.zst" 'idempotent bundle removed'
assert_absent "$SPOOL/host-a/workspace-a/bead-123/attempt-1.json" 'idempotent sidecar removed'

# A missing TLS guard must fail before contacting the sink. The local test
# endpoint is HTTP, so only the explicit test flag permits it.
set +e
"$DRAIN" --spool-dir "$SPOOL" --rclone-config "$RCLONE_CONFIG" \
  --remote sink --bucket archive >"$TMP_DIR/tls.out" 2>&1
tls_status=$?
set -e
[[ "$tls_status" -ne 0 ]] || { echo 'FAIL: insecure endpoint was accepted' >&2; exit 1; }
grep -q 'not TLS' "$TMP_DIR/tls.out"

# Point the same mode-600 config at an unused local port. A failed upload must
# leave its pair in the spool and make the drain fail, while still writing the
# doctor status file.
BAD_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
sed "s#endpoint = .*#endpoint = http://127.0.0.1:$BAD_PORT#" "$RCLONE_CONFIG" >"$TMP_DIR/bad.conf"
chmod 600 "$TMP_DIR/bad.conf"
make_pair attempt-failed
set +e
ATTEMPT_ARCHIVE_RCLONE_RETRIES=1 \
ATTEMPT_ARCHIVE_RCLONE_RETRY_SLEEP=0s \
ATTEMPT_ARCHIVE_RCLONE_LOW_LEVEL_RETRIES=1 \
timeout 30 "$DRAIN" --spool-dir "$SPOOL" --rclone-config "$TMP_DIR/bad.conf" \
  --remote sink --bucket archive --allow-insecure >"$TMP_DIR/failure.out" 2>&1
failure_status=$?
set -e
[[ "$failure_status" -ne 0 ]] || { echo 'FAIL: failed upload returned success' >&2; exit 1; }
assert_file "$SPOOL/host-a/workspace-a/bead-123/attempt-failed.tar.zst" 'failed bundle retained'
assert_file "$SPOOL/host-a/workspace-a/bead-123/attempt-failed.json" 'failed sidecar retained'
assert_eq "$(jq -r '.failed' "$SPOOL/last-drain.json")" 1 'failed drain count'

echo 'attempt archive drain tests passed'
