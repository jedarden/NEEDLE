#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
import sys
sys.path.insert(0, "src")
import config
import formatting
import report

assert config.format_status is formatting.format_status
assert report.format_status is formatting.format_status
assert config.format_status("ready", "yes") == "ready: yes"
assert report.format_status("ready", "yes") == "ready: yes"
PY
