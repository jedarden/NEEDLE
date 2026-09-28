#!/usr/bin/env bash
set -euo pipefail

ROOT="$(mktemp -d /tmp/glm53-canary-test.XXXXXX)"
trap 'find "$ROOT" -depth -delete' EXIT

GOOD="$ROOT/good.json"
python3 scripts/glm53_flash_canary.py --profile good --output "$GOOD"
python3 - "$GOOD" <<'PY'
import json
import sys

result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["result_schema"] == "glm53-canary-result-v1"
assert result["summary"]["quality_guardrails"]["passed"] is True
assert result["summary"]["scoring"]["verified_successes"] == 12
assert result["summary"]["scoring"]["excluded_control_plane_attempts"] == 0
assert result["isolation"]["temporary_home"] is True
assert result["isolation"]["production_workspace_discovery_blocked"] is True

task_rows = [row for row in result["rows"] if row["task_id"] != "alias-audit"]
assert len(task_rows) == 12
assert len({row["assignment_id"] for row in task_rows}) == 12
assert all(row["effective_model"] == "glm-5.3-flash" for row in task_rows)
assert all("prompt_body" not in row and "reasoning_trace" not in row for row in result["rows"])
alias_rows = [row for row in result["rows"] if row["task_id"] == "alias-audit"]
assert {row["requested_model"] for row in alias_rows} == {"glm-4.7", "glm-5.3-flash"}
assert {row["effective_model"] for row in alias_rows} == {"glm-5.3-flash"}
assert all(row["identity_test"] == "provider_resolution" for row in alias_rows)
PY

BAD="$ROOT/bad.json"
if python3 scripts/glm53_flash_canary.py --profile bad --output "$BAD"; then
    echo "the deliberate bad profile unexpectedly passed" >&2
    exit 1
fi
python3 - "$BAD" <<'PY'
import json
import sys

result = json.load(open(sys.argv[1], encoding="utf-8"))
guardrails = result["summary"]["quality_guardrails"]
assert guardrails["passed"] is False
assert "verified_success_rate_below_threshold" in guardrails["failures"]
assert result["summary"]["scoring"]["false_close_reopens"] > 0
assert result["summary"]["scoring"]["unrelated_mutation_count"] > 0
PY

echo "glm53 canary corpus checks passed"
