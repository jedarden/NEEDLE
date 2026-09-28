#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
import sys
sys.path.insert(0, "src")
from parser import parse_count

assert parse_count(" 42 ") == 42
assert parse_count("7") == 7
PY
