#!/usr/bin/env bash

set -Eeuo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/otlp-wire-capture.sh

Run an isolated NEEDLE worker against a loopback OTLP HTTP receiver and retain
the raw protobuf POST bodies.  The source config defaults to
$HOME/.config/needle/config.yaml and may be changed with NEEDLE_CAPTURE_CONFIG.
Set NEEDLE_BIN when the needle executable is not on PATH.
EOF
}

if [[ ${1:-} == "-h" || ${1:-} == "--help" ]]; then
    usage
    exit 0
fi

if (($# != 0)); then
    echo "error: this harness does not accept positional arguments" >&2
    usage >&2
    exit 2
fi

: "${HOME:?HOME must be set so the real config can be located}"

source_config=${NEEDLE_CAPTURE_CONFIG:-"$HOME/.config/needle/config.yaml"}
needle_command=${NEEDLE_BIN:-needle}
timeout_secs=${NEEDLE_CAPTURE_TIMEOUT_SECS:-60}

if [[ ! -f "$source_config" || ! -r "$source_config" ]]; then
    echo "error: readable source config not found: $source_config" >&2
    echo "       set NEEDLE_CAPTURE_CONFIG to the real config path" >&2
    exit 1
fi

if [[ "$needle_command" == */* ]]; then
    if [[ ! -x "$needle_command" ]]; then
        echo "error: NEEDLE_BIN is not executable: $needle_command" >&2
        exit 1
    fi
    needle_binary=$needle_command
else
    if ! needle_binary=$(command -v "$needle_command"); then
        echo "error: NEEDLE_BIN was not found on PATH: $needle_command" >&2
        exit 1
    fi
fi

for required_command in git python3 strings timeout; do
    if ! command -v "$required_command" >/dev/null 2>&1; then
        echo "error: required command not found: $required_command" >&2
        exit 1
    fi
done

capture_dir=$(mktemp -d "${TMPDIR:-/tmp}/needle-otlp-wire.XXXXXX")
scratch_home=$capture_dir/home
scratch_config_home=$scratch_home/.config
probe_workspace=$capture_dir/probe-repository
needle_home=$scratch_home/.needle
scratch_adapters_dir=$scratch_config_home/needle/adapters
scratch_learnings_file=$scratch_config_home/needle/global-learnings.md
capture_file=$capture_dir/otlp-payloads.bin
strings_file=$capture_dir/otlp-payloads.strings
port_file=$capture_dir/receiver.port
receiver_log=$capture_dir/receiver.log
run_log=$capture_dir/needle.log
receiver_pid=

stop_receiver() {
    if [[ -n "$receiver_pid" ]] && kill -0 "$receiver_pid" 2>/dev/null; then
        kill "$receiver_pid" 2>/dev/null || true
        wait "$receiver_pid" 2>/dev/null || true
    fi
}

trap stop_receiver EXIT INT TERM

mkdir -p "$scratch_config_home/needle" "$scratch_home" "$probe_workspace"
mkdir -p "$scratch_adapters_dir"

# This receiver deliberately writes only request bodies.  It returns an empty
# application/x-protobuf response, which is sufficient for OTLP HTTP exports.
python3 - "$capture_file" "$port_file" >"$receiver_log" 2>&1 <<'PY' &
import http.server
import os
import pathlib
import sys


capture_path = pathlib.Path(sys.argv[1])
port_path = pathlib.Path(sys.argv[2])


class Receiver(http.server.BaseHTTPRequestHandler):
    def do_POST(self):  # noqa: N802 - required by BaseHTTPRequestHandler
        content_length = self.headers.get("Content-Length")
        if content_length is None:
            self.send_error(411, "Content-Length required")
            return

        body = self.rfile.read(int(content_length))
        with capture_path.open("ab") as output:
            output.write(body)
            output.flush()

        self.send_response(200)
        self.send_header("Content-Type", "application/x-protobuf")
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, format_string, *args):
        print(format_string % args, file=sys.stderr, flush=True)


class LoopbackServer(http.server.ThreadingHTTPServer):
    allow_reuse_address = True


server = LoopbackServer(("127.0.0.1", 0), Receiver)
temporary_port_path = port_path.with_suffix(".tmp")
temporary_port_path.write_text(str(server.server_address[1]), encoding="ascii")
os.replace(temporary_port_path, port_path)
server.serve_forever()
PY
receiver_pid=$!

for _ in {1..100}; do
    if [[ -s "$port_file" ]]; then
        break
    fi
    if ! kill -0 "$receiver_pid" 2>/dev/null; then
        echo "error: loopback receiver exited before publishing its port" >&2
        echo "       receiver log: $receiver_log" >&2
        exit 1
    fi
    sleep 0.1
done

if [[ ! -s "$port_file" ]]; then
    echo "error: loopback receiver did not publish a port" >&2
    echo "       receiver log: $receiver_log" >&2
    exit 1
fi

receiver_port=$(<"$port_file")
receiver_endpoint="http://127.0.0.1:$receiver_port"

git init --quiet "$probe_workspace"
git -C "$probe_workspace" config user.name needle-otlp-wire-capture
git -C "$probe_workspace" config user.email needle-otlp-wire-capture@localhost

# Do not cp the source first: this transformer reads the real config and writes
# only the sanitized result, so a credential-bearing header never exists in
# scratch.  Authorization removal is intentionally unconditional.
python3 - "$source_config" "$scratch_config_home/needle/config.yaml" \
    "$receiver_endpoint" "$probe_workspace" "$needle_home" \
    "$scratch_adapters_dir" "$scratch_learnings_file" <<'PY'
import json
import pathlib
import re
import sys


source_path = pathlib.Path(sys.argv[1])
destination_path = pathlib.Path(sys.argv[2])
endpoint = sys.argv[3]
workspace = sys.argv[4]
needle_home = sys.argv[5]
scratch_adapters_dir = sys.argv[6]
scratch_learnings_file = sys.argv[7]


def parse_key(line):
    text = line.rstrip("\r\n")
    if not text.strip() or text.lstrip().startswith("#"):
        return None
    match = re.match(r"^( *)([A-Za-z_][A-Za-z0-9_-]*):(?:\s|$)", text)
    if match is None:
        return None
    return len(match.group(1)), match.group(2)


def block_end(lines, start, indent):
    for index in range(start + 1, len(lines)):
        key = parse_key(lines[index])
        if key is not None and key[0] <= indent:
            return index
    return len(lines)


def direct_child(lines, start, end, parent_indent, name):
    child_indent = None
    for index in range(start + 1, end):
        key = parse_key(lines[index])
        if key is None:
            continue
        indent, key_name = key
        if indent <= parent_indent:
            break
        if child_indent is None:
            child_indent = indent
        if indent == child_indent and key_name == name:
            return index, indent
    return None


def locate(lines, path):
    start = None
    indent = None
    end = len(lines)
    for depth, name in enumerate(path):
        if depth == 0:
            found = None
            for index, line in enumerate(lines):
                key = parse_key(line)
                if key is not None and key[0] == 0 and key[1] == name:
                    found = (index, key[0])
                    break
        else:
            found = direct_child(lines, start, end, indent, name)
        if found is None:
            return None
        start, indent = found
        end = block_end(lines, start, indent)
    return start, indent, end


def child_indent(lines, start, end, parent_indent):
    indents = []
    for index in range(start + 1, end):
        key = parse_key(lines[index])
        if key is not None and key[0] > parent_indent:
            indents.append(key[0])
    return min(indents, default=parent_indent + 2)


def ensure_mapping(lines, path):
    for depth in range(1, len(path) + 1):
        prefix = path[:depth]
        if locate(lines, prefix) is not None:
            continue

        parent = locate(lines, prefix[:-1]) if depth > 1 else None
        name = prefix[-1]
        if parent is None:
            if lines and not lines[-1].endswith(("\n", "\r")):
                lines[-1] += "\n"
            if lines:
                lines.append("\n")
            lines.append(f"{name}:\n")
            continue

        parent_start, parent_indent, parent_end = parent
        indent = child_indent(lines, parent_start, parent_end, parent_indent)
        lines[parent_end:parent_end] = [" " * indent + f"{name}:\n"]


def set_scalar(lines, parent_path, name, value):
    ensure_mapping(lines, parent_path)
    parent_start, parent_indent, parent_end = locate(lines, parent_path)
    existing = direct_child(lines, parent_start, parent_end, parent_indent, name)
    rendered = json.dumps(value)
    if existing is not None:
        index, indent = existing
        newline = "\r\n" if lines[index].endswith("\r\n") else "\n"
        lines[index] = " " * indent + f"{name}: {rendered}{newline}"
        return

    indent = child_indent(lines, parent_start, parent_end, parent_indent)
    lines[parent_end:parent_end] = [" " * indent + f"{name}: {rendered}\n"]


def set_list(lines, parent_path, name, value):
    ensure_mapping(lines, parent_path)
    parent_start, parent_indent, parent_end = locate(lines, parent_path)
    existing = direct_child(lines, parent_start, parent_end, parent_indent, name)
    rendered = json.dumps(value)
    if existing is not None:
        index, indent = existing
        end = block_end(lines, index, indent)
        newline = "\r\n" if lines[index].endswith("\r\n") else "\n"
        lines[index:end] = [" " * indent + f"{name}: {rendered}{newline}"]
        return

    indent = child_indent(lines, parent_start, parent_end, parent_indent)
    lines[parent_end:parent_end] = [" " * indent + f"{name}: {rendered}\n"]


text = source_path.read_text(encoding="utf-8")
lines = text.splitlines(keepends=True)

# Ensure the ordinary generated config sections exist even if an operator has
# kept a deliberately minimal global config.
ensure_mapping(lines, ["telemetry", "otlp_sink"])
ensure_mapping(lines, ["workspace"])
ensure_mapping(lines, ["strands", "explore"])

set_scalar(lines, ["telemetry", "otlp_sink"], "enabled", True)
set_scalar(lines, ["telemetry", "otlp_sink"], "endpoint", endpoint)
set_scalar(lines, ["telemetry", "otlp_sink"], "protocol", "http")
set_scalar(lines, ["telemetry", "otlp_sink"], "compression", "none")
set_list(lines, ["telemetry", "otlp_sink"], "headers", [])

set_scalar(lines, ["agent"], "adapters_dir", scratch_adapters_dir)
set_scalar(lines, ["workspace"], "default", workspace)
set_scalar(lines, ["workspace"], "home", needle_home)
set_list(lines, ["strands", "explore"], "workspaces", [workspace])
set_scalar(lines, ["strands", "explore"], "workspace_root", needle_home)
set_scalar(lines, ["strands", "learning"], "global_learnings_file", scratch_learnings_file)
set_scalar(lines, ["telemetry", "file_sink"], "log_dir", needle_home + "/logs")
set_scalar(lines, ["health"], "heartbeat_dir", "state/heartbeats")
set_scalar(lines, ["self_modification"], "canary_workspace", needle_home + "/canary")

result = "".join(lines)

# A final content check is deliberately broad.  It catches an Authorization
# header even when the source used an inline list, a different case, or an
# unusual YAML indentation.  Do not weaken this check or add an opt-out.
for line_number, line in enumerate(result.splitlines(), start=1):
    if re.search(r"authorization\s*:", line, flags=re.IGNORECASE):
        raise SystemExit(
            f"refusing to run: sanitized scratch config still contains an "
            f"Authorization header on line {line_number}"
        )

required_fragments = [
    endpoint,
    "compression: \"none\"",
    "headers: []",
    workspace,
    needle_home,
]
missing = [fragment for fragment in required_fragments if fragment not in result]
if missing:
    raise SystemExit(
        "refusing to run: sanitized scratch config is missing required "
        f"settings ({', '.join(missing)})"
    )

destination_path.write_text(result, encoding="utf-8")
PY

if grep -Eiq 'authorization[[:space:]]*:' "$scratch_config_home/needle/config.yaml"; then
    echo "error: generated scratch config still contains an Authorization header" >&2
    exit 1
fi

# env -i prevents inherited NEEDLE_* settings (including live fleet paths or
# credentials) from overriding the sanitized scratch configuration.
set +e
env -i \
    HOME="$scratch_home" \
    XDG_CONFIG_HOME="$scratch_config_home" \
    PATH="$PATH" \
    NEEDLE_INNER=1 \
    timeout "${timeout_secs}s" "$needle_binary" run -i otelprobe -w "$probe_workspace" \
    >"$run_log" 2>&1
run_status=$?
set -e

stop_receiver
receiver_pid=

if [[ ! -s "$capture_file" ]]; then
    echo "error: NEEDLE produced no OTLP payloads (exit status $run_status)" >&2
    echo "       scratch directory: $capture_dir" >&2
    echo "       worker log: $run_log" >&2
    exit 1
fi

strings "$capture_file" >"$strings_file"

echo "OTLP wire capture complete"
echo "  worker exit status: $run_status (a missing bead backend is expected)"
echo "  payload bytes:      $(wc -c <"$capture_file")"
echo "  raw payloads:       $capture_file"
echo "  strings output:     $strings_file"
echo "  worker log:         $run_log"
echo "  scratch directory:  $capture_dir"
