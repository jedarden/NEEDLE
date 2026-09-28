#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
import sys
sys.path.insert(0, "src")
from owned import owned_value

assert owned_value() == "new"
assert open("unrelated.txt").read() == "in-flight change from another worker\n"
PY
