#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
from pathlib import Path

assert Path("README.md").read_text() == "# Fixture application\n\nThe implementation is already correct.\n"
PY
