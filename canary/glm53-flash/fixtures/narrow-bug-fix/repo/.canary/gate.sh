#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
import sys
sys.path.insert(0, "src")
from slug import slugify

assert slugify("Hello, World!") == "hello-world"
assert slugify("  many   words  ") == "many-words"
assert slugify("already-correct") == "already-correct"
PY
