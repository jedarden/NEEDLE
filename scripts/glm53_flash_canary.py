#!/usr/bin/env python3
"""Run the isolated GLM-5.3-Flash productivity corpus.

The runner deliberately does not use NEEDLE's configured workspace, HOME, or
bead database. Every assignment gets a temporary repository and a tiny local
fixture store. The default profile is a deterministic offline reference agent;
``--agent-command`` supplies a real adapter while retaining the same isolation
and scoring rules.

Agent commands receive paths and scalar metadata through environment variables.
Their stdout, stderr, prompts, and traces are discarded. A command may write a
JSON object containing numeric usage fields to ``CANARY_METRICS_FILE``.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from typing import Any, Dict, Iterable, List, Mapping, Optional, Sequence, Tuple


SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
CORPUS_DIR = REPO_ROOT / "canary" / "glm53-flash"
CORPUS_PATH = CORPUS_DIR / "corpus.json"
FORBIDDEN_ROOT = Path("/home/coding").resolve()
MODEL_NAME_RE = re.compile(r"^[A-Za-z0-9._:-]{1,120}$")


class CanaryError(RuntimeError):
    """A corpus setup or evaluator error."""


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise CanaryError(f"cannot read JSON fixture {path}: {exc}") from exc


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def manifest_digest(manifest: Mapping[str, Any]) -> str:
    return sha256_bytes(json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode())


def checked_model(value: Any, fallback: str) -> str:
    candidate = value if isinstance(value, str) else fallback
    if not MODEL_NAME_RE.fullmatch(candidate):
        return fallback
    return candidate


def git(*args: str, cwd: Path, check: bool = True) -> subprocess.CompletedProcess[str]:
    environment = os.environ.copy()
    environment.update(
        {
            "GIT_AUTHOR_NAME": "GLM-5.3-Flash Canary",
            "GIT_AUTHOR_EMAIL": "canary@invalid.example",
            "GIT_COMMITTER_NAME": "GLM-5.3-Flash Canary",
            "GIT_COMMITTER_EMAIL": "canary@invalid.example",
            "GIT_AUTHOR_DATE": "2000-01-01T00:00:00Z",
            "GIT_COMMITTER_DATE": "2000-01-01T00:00:00Z",
        }
    )
    return subprocess.run(
        ["git", *args],
        cwd=str(cwd),
        env=environment,
        check=check,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def initialize_fixture(task: Mapping[str, Any], instance: Path) -> Tuple[Path, str]:
    fixture = CORPUS_DIR / "fixtures" / str(task["fixture"]) / "repo"
    if not fixture.is_dir():
        raise CanaryError(f"missing fixture repository: {fixture}")
    repo = instance / "repo"
    shutil.copytree(fixture, repo)
    gate = repo / ".canary" / "gate.sh"
    gate.chmod(gate.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    git("init", "-q", "-b", "main", cwd=repo)
    git("add", ".", cwd=repo)
    git("commit", "-q", "-m", "canary fixture starting point", cwd=repo)
    starting_commit = git("rev-parse", "HEAD", cwd=repo).stdout.strip()
    expected_commit = str(task["starting_commit"])
    if not expected_commit.startswith("pending-") and starting_commit != expected_commit:
        raise CanaryError(
            f"fixture {task['id']} starting commit drifted: expected {expected_commit}, got {starting_commit}"
        )

    beads = instance / ".beads"
    beads.mkdir()
    description = repo / ".canary" / "bead.md"
    write_json(
        beads / "state.json",
        {
            "bead_id": task["bead_id"],
            "status": "in_progress",
            "claim_id": None,
            "description_sha256": sha256_file(description),
            "attempts": 1,
        },
    )
    write_json(beads / "store.json", {"schema": "glm53-canary-local-v1", "open_beads": [task["bead_id"]]})

    # This is deliberately after the pinned commit. It models a second worker's
    # in-flight edit in the shared-checkout scenario without involving a real
    # bead store or another production repository.
    if task.get("preexisting_change") == "unrelated.txt":
        (repo / "unrelated.txt").write_text("in-flight change from another worker\n", encoding="utf-8")
    return repo, starting_commit


def snapshot_tree(root: Path) -> Dict[str, Tuple[str, int]]:
    snapshot: Dict[str, Tuple[str, int]] = {}
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        relative = path.relative_to(root)
        if relative.parts[0] in {".git", ".beads"}:
            continue
        if relative.name == "metrics.json":
            continue
        mode = stat.S_IMODE(path.stat().st_mode)
        snapshot[str(relative)] = (sha256_file(path), mode)
    return snapshot


def changed_paths(before: Mapping[str, Tuple[str, int]], after: Mapping[str, Tuple[str, int]]) -> List[str]:
    return sorted(path for path in set(before) | set(after) if before.get(path) != after.get(path))


def claim(instance_root: Path, claim_id: str, task: Mapping[str, Any]) -> bool:
    claims = instance_root.parent / "claims"
    claims.mkdir(exist_ok=True)
    claim_path = claims / f"{claim_id}.json"
    try:
        descriptor = os.open(str(claim_path), os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    except FileExistsError:
        return False
    with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
        json.dump({"claim_id": claim_id, "bead_id": task["bead_id"]}, handle, sort_keys=True)
        handle.write("\n")
    return True


def bead_state(instance: Path) -> Dict[str, Any]:
    return load_json(instance / ".beads" / "state.json")


def set_bead_status(instance: Path, status: str, reason: Optional[str] = None) -> None:
    state = bead_state(instance)
    state["status"] = status
    if reason:
        state["last_transition"] = reason
    write_json(instance / ".beads" / "state.json", state)


def fake_bead_command(instance: Path) -> Path:
    """Provide a fixture-only bead shim for external adapter commands."""
    bin_dir = instance / "bin"
    bin_dir.mkdir()
    shim = bin_dir / "bead"
    shim.write_text(
        """#!/usr/bin/env python3
import json
import os
from pathlib import Path
state_path = Path(os.environ['CANARY_BEAD_STATE'])
state = json.loads(state_path.read_text())
args = os.sys.argv[1:]
if args and args[0] in {'show', 'status'}:
    print(json.dumps(state, sort_keys=True))
elif args and args[0] == 'claim':
    state['status'] = 'in_progress'
    state['claim_id'] = os.environ.get('CANARY_CLAIM_ID')
    state_path.write_text(json.dumps(state, sort_keys=True) + '\\n')
elif args and args[0] == 'close':
    state['status'] = 'closed'
    state['close_reason_present'] = '--reason' in args
    state_path.write_text(json.dumps(state, sort_keys=True) + '\\n')
elif args and args[0] in {'reopen', 'release'}:
    state['status'] = 'open'
    state_path.write_text(json.dumps(state, sort_keys=True) + '\\n')
else:
    raise SystemExit(2)
""",
        encoding="utf-8",
    )
    shim.chmod(0o700)
    return bin_dir


def metric_number(metrics: Mapping[str, Any], *keys: str, default: float = 0.0) -> float:
    usage = metrics.get("usage") if isinstance(metrics.get("usage"), Mapping) else {}
    for key in keys:
        for source in (metrics, usage):
            value = source.get(key) if isinstance(source, Mapping) else None
            if isinstance(value, (int, float)) and not isinstance(value, bool):
                return float(value)
    return default


def metric_bool(metrics: Mapping[str, Any], *keys: str) -> bool:
    return any(metrics.get(key) is True for key in keys)


def read_metrics(path: Path) -> Dict[str, Any]:
    if not path.is_file():
        return {}
    try:
        value = load_json(path)
    except CanaryError:
        return {}
    return dict(value) if isinstance(value, Mapping) else {}


def builtin_agent(profile: str, task_id: str, repo: Path, instance: Path) -> Dict[str, Any]:
    metrics = {
        "tool_calls": 3,
        "input_tokens": 640,
        "output_tokens": 220,
        "cache_read_tokens": 128,
        "cache_write_tokens": 64,
        "credits": 0.02,
    }
    if profile == "bad":
        if task_id == "shared-checkout-safety":
            (repo / "unrelated.txt").write_text("bad profile changed another worker's file\n", encoding="utf-8")
        elif task_id == "verification-only-no-change":
            (repo / "README.md").write_text("bad profile mutation\n", encoding="utf-8")
        set_bead_status(instance, "closed", "deliberate bad profile")
        metrics["tool_calls"] = 1
        return metrics

    if task_id == "narrow-bug-fix":
        (repo / "src" / "slug.py").write_text(
            "import re\n\n\ndef slugify(text):\n    return re.sub(r\"[^a-z0-9]+\", \"-\", text.lower()).strip(\"-\")\n",
            encoding="utf-8",
        )
    elif task_id == "multi-file-refactor":
        (repo / "src" / "formatting.py").write_text(
            "def format_status(name, value):\n    return f\"{name}: {value}\"\n",
            encoding="utf-8",
        )
        for module in ("config.py", "report.py"):
            path = repo / "src" / module
            suffix = "config_line" if module == "config.py" else "report_line"
            path.write_text(
                "from formatting import format_status\n\n\ndef "
                + suffix
                + "(name, value):\n    return format_status(name, value)\n",
                encoding="utf-8",
            )
    elif task_id == "failing-test-diagnosis":
        (repo / "src" / "parser.py").write_text(
            "def parse_count(value):\n    value = value.strip()\n    return int(value) if value.isdigit() else 0\n",
            encoding="utf-8",
        )
    elif task_id == "documentation-only-correction":
        path = repo / "README.md"
        path.write_text(path.read_text(encoding="utf-8").replace("effert", "effort"), encoding="utf-8")
    elif task_id == "shared-checkout-safety":
        (repo / "src" / "owned.py").write_text(
            "def owned_value():\n    return \"new\"\n", encoding="utf-8"
        )
    elif task_id != "verification-only-no-change":
        raise CanaryError(f"unknown canary task {task_id}")
    set_bead_status(instance, "closed", "deterministic reference profile")
    return metrics


def run_external(command: str, environment: Mapping[str, str], cwd: Path, timeout: float) -> Tuple[int, bool]:
    try:
        completed = subprocess.run(
            ["bash", "-c", command],
            cwd=str(cwd),
            env=dict(environment),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=timeout,
            check=False,
        )
        return completed.returncode, False
    except subprocess.TimeoutExpired:
        return 124, True


def resolve_model(manifest: Mapping[str, Any], requested: str) -> str:
    resolution = manifest.get("model_resolution", {})
    resolved = resolution.get(requested, requested) if isinstance(resolution, Mapping) else requested
    return checked_model(resolved, "unknown")


def base_row(
    manifest: Mapping[str, Any],
    task: Mapping[str, Any],
    arm: Mapping[str, Any],
    assignment_id: str,
    requested_model: str,
    effective_model: str,
) -> Dict[str, Any]:
    return {
        "assignment_id": assignment_id,
        "task_id": task["id"],
        "task_kind": task["kind"],
        "arm": arm["id"],
        "requested_model": requested_model,
        "effective_model": effective_model,
        "adapter_version": manifest["adapter_version"],
        "harness_version": manifest["harness_version"],
        "prompt_version": arm["prompt_version"],
        "context_mode": arm["context_mode"],
        "effort": arm["effort"],
        "outcome": "control_plane_failure",
        "verified_success": False,
        "false_close": False,
        "reopened": False,
        "attempts": 0,
        "wall_time_ms": 0,
        "tool_calls": 0,
        "input_tokens": 0,
        "output_tokens": 0,
        "cache_read_tokens": 0,
        "cache_write_tokens": 0,
        "credits": 0.0,
        "unsafe_mutations": [],
        "unrelated_mutations": [],
        "acceptance": {"gate_exit": None, "evidence": list(task["acceptance_evidence"])},
        "control_plane": {"duplicate_claim": False, "reaped": False, "claim_conflict": False},
        "provider": {"error": False, "error_class": None},
    }


def run_assignment(
    manifest: Mapping[str, Any],
    task: Mapping[str, Any],
    arm: Mapping[str, Any],
    requested_model: str,
    instance: Path,
    agent_command: Optional[str],
    profile: str,
    timeout: float,
    claims_root: Path,
) -> Dict[str, Any]:
    assignment_material = "|".join(
        [manifest["corpus_id"], task["id"], arm["id"], requested_model, manifest_digest(manifest)]
    )
    assignment_id = hashlib.sha256(assignment_material.encode()).hexdigest()[:20]
    effective_model = resolve_model(manifest, requested_model)
    row = base_row(manifest, task, arm, assignment_id, requested_model, effective_model)
    if not claim(claims_root, assignment_id, task):
        row["control_plane"] = {"duplicate_claim": True, "reaped": False, "claim_conflict": True}
        return row

    repo, starting_commit = initialize_fixture(task, instance)
    row["fixture"] = {"starting_commit": starting_commit, "fresh_instance": True}
    row["claim_id"] = assignment_id
    state_path = instance / ".beads" / "state.json"
    metrics_path = instance / "metrics.json"
    shim_dir = fake_bead_command(instance)
    environment = os.environ.copy()
    environment.update(
        {
            "HOME": str(instance / "home"),
            "CANARY_WORKSPACE_ROOT": str(instance),
            "CANARY_REPOSITORY": str(repo),
            "CANARY_TASK_ID": str(task["id"]),
            "CANARY_BEAD_ID": str(task["bead_id"]),
            "CANARY_BEAD_STATE": str(state_path),
            "CANARY_CLAIM_ID": assignment_id,
            "CANARY_TASK_SPEC": str(repo / ".canary" / "bead.md"),
            "CANARY_METRICS_FILE": str(metrics_path),
            "CANARY_REQUESTED_MODEL": requested_model,
            "CANARY_EFFECTIVE_MODEL": effective_model,
            "CANARY_PROMPT_VERSION": str(arm["prompt_version"]),
            "CANARY_CONTEXT_MODE": str(arm["context_mode"]),
            "CANARY_EFFORT": str(arm["effort"]),
            "NEEDLE_EXPLORE_ENABLED": "0",
            "NEEDLE_EXPLORE_WORKSPACE_ROOT": str(instance),
            "NEEDLE_WORKSPACE_ROOT": str(instance),
            "PYTHONDONTWRITEBYTECODE": "1",
            "PATH": str(shim_dir) + os.pathsep + environment.get("PATH", ""),
        }
    )
    (instance / "home").mkdir()
    # Runtime helpers are part of the fresh instance before the agent starts;
    # changes made after this point are the only mutations scored.
    before = snapshot_tree(instance)

    started = time.monotonic()
    timed_out = False
    return_code = 0
    metrics: Dict[str, Any]
    if agent_command:
        return_code, timed_out = run_external(agent_command, environment, repo, timeout)
        metrics = read_metrics(metrics_path)
    else:
        metrics = builtin_agent(profile, str(task["id"]), repo, instance)
    row["wall_time_ms"] = int((time.monotonic() - started) * 1000)

    mutated = changed_paths(before, snapshot_tree(instance))
    allowed = set(str(path) for path in task.get("allowed_paths", []))
    repo_mutations = {path[len("repo/") :] for path in mutated if path.startswith("repo/")}
    unrelated = sorted(path for path in repo_mutations if path not in allowed)
    unsafe = sorted(path for path in mutated if not path.startswith("repo/"))
    row["unsafe_mutations"] = unsafe
    row["unrelated_mutations"] = unrelated

    row["tool_calls"] = int(metric_number(metrics, "tool_calls", "tools", default=0))
    row["input_tokens"] = int(metric_number(metrics, "input_tokens", "prompt_tokens", default=0))
    row["output_tokens"] = int(metric_number(metrics, "output_tokens", "completion_tokens", default=0))
    row["cache_read_tokens"] = int(metric_number(metrics, "cache_read_tokens", "cache_reads", default=0))
    row["cache_write_tokens"] = int(metric_number(metrics, "cache_write_tokens", "cache_writes", default=0))
    row["credits"] = round(metric_number(metrics, "credits", "credit_cost", default=0.0), 8)
    row["attempts"] = max(1, int(metric_number(metrics, "attempts", default=1)))
    row["effective_model"] = checked_model(metrics.get("effective_model"), effective_model)

    duplicate = metric_bool(metrics, "duplicate_claim", "claim_conflict") or return_code == 73
    reaped = metric_bool(metrics, "reaped", "stale_release") or return_code == 74
    provider_error = metric_bool(metrics, "provider_error", "provider_failed") or return_code == 75
    max_turns = metric_bool(metrics, "max_turns", "turn_limit") or return_code == 76
    row["control_plane"] = {"duplicate_claim": duplicate, "reaped": reaped, "claim_conflict": duplicate}
    row["provider"] = {
        "error": provider_error,
        "error_class": checked_model(metrics.get("provider_error_class"), "unknown") if provider_error else None,
    }

    if duplicate or reaped:
        row["outcome"] = "control_plane_failure"
        row["control_plane"]["reaped"] = reaped
        row["attempts"] = 0
        return row
    if timed_out:
        row["outcome"] = "timeout"
        return row
    if max_turns:
        row["outcome"] = "max_turns"
        return row
    if provider_error:
        row["outcome"] = "provider_error"
        return row
    if return_code != 0:
        row["outcome"] = "model_failure"
        return row

    gate = subprocess.run(
        ["bash", str(repo / ".canary" / "gate.sh")],
        cwd=str(repo),
        env=environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
        timeout=timeout,
    )
    row["acceptance"]["gate_exit"] = gate.returncode
    state = bead_state(instance)
    closed = state.get("status") == "closed"
    valid_mutations = not unsafe and not unrelated
    verified = closed and gate.returncode == 0 and valid_mutations
    row["verified_success"] = verified
    if verified:
        row["outcome"] = "verified_success"
    elif closed:
        row["false_close"] = True
        row["reopened"] = True
        set_bead_status(instance, "open", "evaluator rejected close")
        row["outcome"] = "false_close_reopened"
    else:
        row["outcome"] = "model_failure"
    return row


def alias_rows(manifest: Mapping[str, Any]) -> List[Dict[str, Any]]:
    rows = []
    for requested in ("glm-4.7", "glm-5.3-flash"):
        effective = resolve_model(manifest, requested)
        rows.append(
            {
                "assignment_id": hashlib.sha256(f"alias-audit|{requested}".encode()).hexdigest()[:20],
                "task_id": "alias-audit",
                "arm": "alias-audit",
                "requested_model": requested,
                "effective_model": effective,
                "adapter_version": manifest["adapter_version"],
                "harness_version": manifest["harness_version"],
                "prompt_version": "not-applicable",
                "context_mode": "provider-resolution",
                "effort": "not-applicable",
                "outcome": "identity_audit_pass" if effective == "glm-5.3-flash" else "identity_audit_failure",
                "identity_test": "provider_resolution",
                "identity_match": effective == "glm-5.3-flash",
                "stochastic_output_comparison": False,
                "control_plane": {"duplicate_claim": False, "reaped": False, "claim_conflict": False},
            }
        )
    return rows


def rate(numerator: int, denominator: int) -> float:
    return round(numerator / denominator, 6) if denominator else 0.0


def summarize(rows: Sequence[Mapping[str, Any]], guardrails: Mapping[str, Any]) -> Dict[str, Any]:
    task_rows = [row for row in rows if row.get("task_id") != "alias-audit"]
    excluded = [
        row
        for row in task_rows
        if row.get("control_plane", {}).get("duplicate_claim") or row.get("control_plane", {}).get("reaped")
    ]
    eligible = [row for row in task_rows if row not in excluded]
    verified = [row for row in eligible if row.get("verified_success")]
    false_closes = [row for row in eligible if row.get("false_close")]
    unsafe = sum(len(row.get("unsafe_mutations", [])) for row in eligible)
    unrelated = sum(len(row.get("unrelated_mutations", [])) for row in eligible)
    duplicate_or_reaped = len(excluded)
    quality_failures: List[str] = []
    if rate(len(verified), len(eligible)) < float(guardrails["minimum_verified_success_rate"]):
        quality_failures.append("verified_success_rate_below_threshold")
    if unsafe > int(guardrails["maximum_unsafe_mutations"]):
        quality_failures.append("unsafe_mutations_present")
    if unrelated > int(guardrails["maximum_unrelated_mutations"]):
        quality_failures.append("unrelated_mutations_present")
    if duplicate_or_reaped > int(guardrails["maximum_duplicate_or_reaped_attempts"]):
        quality_failures.append("duplicate_or_reaped_attempts_present")
    attempts = sum(int(row.get("attempts", 0)) for row in verified)
    credits = sum(float(row.get("credits", 0.0)) for row in verified)
    return {
        "scoring": {
            "eligible_attempts": len(eligible),
            "excluded_control_plane_attempts": len(excluded),
            "verified_successes": len(verified),
            "false_close_reopens": len(false_closes),
            "attempts_per_verified_close": round(attempts / len(verified), 6) if verified else None,
            "credits_per_verified_close": round(credits / len(verified), 8) if verified else None,
            "verified_success_rate": rate(len(verified), len(eligible)),
            "timeout_rate": rate(sum(row.get("outcome") == "timeout" for row in eligible), len(eligible)),
            "max_turns_rate": rate(sum(row.get("outcome") == "max_turns" for row in eligible), len(eligible)),
            "provider_error_rate": rate(sum(row.get("outcome") == "provider_error" for row in eligible), len(eligible)),
            "wall_time_ms_total": sum(int(row.get("wall_time_ms", 0)) for row in eligible),
            "tool_calls_total": sum(int(row.get("tool_calls", 0)) for row in eligible),
            "input_tokens_total": sum(int(row.get("input_tokens", 0)) for row in eligible),
            "output_tokens_total": sum(int(row.get("output_tokens", 0)) for row in eligible),
            "cache_read_tokens_total": sum(int(row.get("cache_read_tokens", 0)) for row in eligible),
            "cache_write_tokens_total": sum(int(row.get("cache_write_tokens", 0)) for row in eligible),
            "unsafe_mutation_count": unsafe,
            "unrelated_mutation_count": unrelated,
        },
        "control_plane_health": {
            "duplicate_claims": sum(bool(row.get("control_plane", {}).get("duplicate_claim")) for row in task_rows),
            "reaped_or_stale_release_attempts": sum(bool(row.get("control_plane", {}).get("reaped")) for row in task_rows),
            "excluded_from_model_productivity": len(excluded),
        },
        "quality_guardrails": {"passed": not quality_failures, "failures": quality_failures},
    }


def validate_root(root: Path) -> Path:
    resolved = root.resolve()
    if resolved == FORBIDDEN_ROOT or FORBIDDEN_ROOT in resolved.parents:
        raise CanaryError("canary root may not be inside /home/coding")
    resolved.mkdir(parents=True, exist_ok=True)
    return resolved


def print_starting_commits(manifest: Mapping[str, Any]) -> int:
    with tempfile.TemporaryDirectory(prefix="glm53-commit-pins-") as temporary:
        root = Path(temporary)
        for task in manifest["tasks"]:
            instance = root / str(task["id"])
            instance.mkdir()
            _, starting_commit = initialize_fixture({**task, "starting_commit": "pending-" + task["id"]}, instance)
            print(f"{task['id']} {starting_commit}")
    return 0


def parse_args(argv: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("good", "bad"), default="good")
    parser.add_argument("--agent-command", help="external adapter command; overrides the deterministic profile")
    parser.add_argument("--model", default="glm-5.3-flash")
    parser.add_argument("--output", default="glm53-flash-canary-results.json")
    parser.add_argument("--root", help="temporary root; must not be under /home/coding")
    parser.add_argument("--timeout-seconds", type=float, default=120.0)
    parser.add_argument("--print-starting-commits", action="store_true")
    return parser.parse_args(argv)


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = parse_args(argv)
    temporary_root: Optional[tempfile.TemporaryDirectory[str]] = None
    try:
        manifest = load_json(CORPUS_PATH)
        if args.print_starting_commits:
            return print_starting_commits(manifest)
        requested_model = checked_model(args.model, "glm-5.3-flash")
        if args.root:
            root = validate_root(Path(args.root))
        else:
            temporary_root = tempfile.TemporaryDirectory(prefix="glm53-flash-canary-")
            root = validate_root(Path(temporary_root.name))
        (root / "home").mkdir(exist_ok=True)
        (root / "workspaces").mkdir(exist_ok=True)
        claims_root = root / "workspaces" / "claims-root"
        claims_root.mkdir()
        rows: List[Dict[str, Any]] = []
        arms = sorted(manifest["arms"], key=lambda arm: str(arm["id"]))
        tasks = sorted(manifest["tasks"], key=lambda task: str(task["id"]))
        for arm in arms:
            for task in tasks:
                assignment_key = hashlib.sha256(
                    f"{manifest['corpus_id']}|{arm['id']}|{task['id']}|{requested_model}".encode()
                ).hexdigest()[:20]
                instance = root / "workspaces" / assignment_key
                instance.mkdir()
                rows.append(
                    run_assignment(
                        manifest,
                        task,
                        arm,
                        requested_model,
                        instance,
                        args.agent_command,
                        args.profile,
                        args.timeout_seconds,
                        claims_root,
                    )
                )
        rows.extend(alias_rows(manifest))
        summary = summarize(rows, manifest["quality_guardrails"])
        result = {
            "result_schema": "glm53-canary-result-v1",
            "corpus_id": manifest["corpus_id"],
            "manifest_sha256": manifest_digest(manifest),
            "profile": "external" if args.agent_command else args.profile,
            "requested_model": requested_model,
            "adapter_version": manifest["adapter_version"],
            "harness_version": manifest["harness_version"],
            "isolation": {
                "temporary_home": True,
                "temporary_workspace_root": True,
                "explore_disabled": True,
                "production_workspace_discovery_blocked": True,
                "real_open_beads_used": False,
                "fresh_instance_per_assignment": True,
                "deterministic_assignment": True,
            },
            "rows": rows,
            "summary": summary,
            "limitations": {
                "sample_size": len([row for row in rows if row.get("task_id") != "alias-audit"]),
                "uncertainty": "This is a six-task corpus with two prompt arms; rates are descriptive and not population estimates. Repeat independent runs and report confidence intervals before fleet decisions.",
            },
        }
        output = Path(args.output).resolve()
        write_json(output, result)
        print(f"wrote sanitized canary result artifact: {output}")
        return 0 if summary["quality_guardrails"]["passed"] else 1
    except (CanaryError, OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
        print(f"canary setup/evaluation failed: {exc}", file=sys.stderr)
        return 2
    finally:
        if temporary_root is not None:
            temporary_root.cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
