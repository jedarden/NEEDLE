#!/usr/bin/env python3
"""Deterministic reachability audit for the NEEDLE fleet.

Every starvation state found on 2026-09-05 was invisible for the same reason:
the fleet's health checks run INSIDE a worker and ask "did I find work?".
That question cannot see work the worker never looks at. This asks the
complement -- "is any non-closed bead unreachable, and why?" -- as a
reconciliation over state that is already on disk.

Nothing here samples, polls, or races. Each predicate is a pure function of
files at rest (bead stores, the explore config, adapter YAML, the systemd
unit set, the event log), so two runs over unchanged inputs give the same
answer, and a violation names the exact rule it broke.

Read-only. Exit 0 = no unreachable work, 1 = findings, 2 = audit error.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path

HOME = Path("/home/coding")
CONFIG = HOME / ".config/needle/config.yaml"
ADAPTER_DIR = HOME / ".config/needle/adapters"
WORKER_ENV_DIR = HOME / ".config/needle/workers"
CHURN_WINDOW = timedelta(hours=6)
# A claim with no dispatch after this long is unmatched, not merely in flight.
CLAIM_GRACE = timedelta(minutes=10)


@dataclass
class Finding:
    rule: str
    scope: str
    subject: str
    detail: str
    count: int = 1


@dataclass
class Workspace:
    path: Path
    listed: bool = False
    pinned_by: list[str] = field(default_factory=list)
    excluded: bool = False


# ── inputs ────────────────────────────────────────────────────────────────

def load_config() -> dict:
    try:
        import yaml
    except ImportError:
        sys.exit("audit: PyYAML required (pip install pyyaml)")
    with CONFIG.open() as fh:
        return yaml.safe_load(fh)


def bead_stores() -> list[Path]:
    """Every bead-rs store at depth 1 under HOME.

    Depth 1 on purpose: recursive discovery finds backups, scratch clones and
    retired trees, and a workspace nobody can `cd` into is not a workspace.
    """
    out = []
    for entry in sorted(HOME.iterdir()):
        if entry.is_dir() and (entry / ".beads/config.json").is_file():
            out.append(entry)
    return out


def list_beads(ws: Path) -> list[dict]:
    proc = subprocess.run(
        ["bead", "list", "--json", "--limit", "10000"],
        cwd=ws, capture_output=True, text=True,
    )
    beads = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            beads.append(value)
        elif isinstance(value, list):
            beads.extend(b for b in value if isinstance(b, dict))
    return beads


def live_worker_identities() -> set[str]:
    """Worker identities that could legitimately hold a claim right now.

    Union of the systemd unit instances and the identifiers passed on live
    command lines -- a claim held by anything outside this set is held by
    nobody.
    """
    ids: set[str] = set()
    units = subprocess.run(
        ["systemctl", "--user", "list-units", "--type=service", "--state=running",
         "needle-worker@*.service", "--no-legend", "--plain"],
        capture_output=True, text=True,
    ).stdout
    for token in units.split():
        if token.startswith("needle-worker@"):
            ids.add(token[len("needle-worker@"):].removesuffix(".service"))
    procs = subprocess.run(["ps", "-eo", "args"], capture_output=True, text=True).stdout
    for line in procs.splitlines():
        match = re.search(r"--identifier\s+(\S+)", line)
        if match:
            ids.add(match.group(1))
    return ids


def workspace_adapter(ws: Path, global_default: str, global_rules: list) -> tuple[str, str]:
    """Resolve the adapter a worker in this workspace will actually construct.

    Returns (adapter_name, source). A workspace-level .needle.yaml wins over
    the global config -- which is exactly how a retired adapter name survived
    the fleet migration in one repo and crash-looped one worker.
    """
    local = ws / ".needle.yaml"
    if local.is_file():
        try:
            import yaml
            cfg = yaml.safe_load(local.read_text()) or {}
        except Exception:
            return global_default, "global (workspace config unparseable)"
        agent = cfg.get("agent") or {}
        rules = ((agent.get("routing") or {}).get("rules")) or []
        for rule in rules:
            if re.fullmatch(rule.get("match_model", ""), "glm-5.3-flash"):
                return rule.get("adapter", ""), f"{local.name} routing rule"
        if agent.get("default"):
            return agent["default"], f"{local.name} agent.default"
    return global_default, "global config"


def adapter_exists(name: str) -> bool:
    if not name:
        return False
    return any((ADAPTER_DIR / f"{name}{ext}").is_file() for ext in (".yaml", ".yml"))


def claim_churn(ws: Path) -> list[tuple[str, int]]:
    """Beads claimed repeatedly that never reached dispatch.

    Deterministic over the event log: a claim is matched by any later
    dispatch for the same bead. Unmatched claims older than the grace period
    are churn, not work in flight.
    """
    log = ws / ".beads/events.jsonl"
    if not log.is_file():
        return []
    now = datetime.now(timezone.utc)
    claims: Counter[str] = Counter()
    dispatched: set[str] = set()
    latest: dict[str, datetime] = {}
    for line in log.read_text(errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
            stamp = datetime.fromisoformat(event["ts"])
        except Exception:
            continue
        if now - stamp > CHURN_WINDOW:
            continue
        bead = event.get("bead")
        if not bead:
            continue
        kind = event.get("event")
        if kind == "claim":
            claims[bead] += 1
            latest[bead] = max(latest.get(bead, stamp), stamp)
        elif kind == "dispatch":
            dispatched.add(bead)
    return [
        (bead, n) for bead, n in claims.items()
        if bead not in dispatched and n > 1 and now - latest[bead] > CLAIM_GRACE
    ]


# ── predicates ────────────────────────────────────────────────────────────

def audit() -> list[Finding]:
    cfg = load_config()
    explore = cfg.get("strands", {}).get("explore", {})
    listed = {Path(p) for p in explore.get("workspaces", [])}
    exclude_labels = set(cfg.get("strands", {}).get("pluck", {}).get("exclude_labels", []) or [])
    agent = cfg.get("agent", {})
    global_default = agent.get("default", "")
    global_rules = ((agent.get("routing") or {}).get("rules")) or []

    pinned: dict[Path, list[str]] = defaultdict(list)
    if WORKER_ENV_DIR.is_dir():
        for env in WORKER_ENV_DIR.glob("*.env"):
            for line in env.read_text(errors="replace").splitlines():
                if line.startswith("NEEDLE_WS="):
                    pinned[Path(line.split("=", 1)[1].strip().strip('"'))].append(env.stem)

    live = live_worker_identities()
    findings: list[Finding] = []

    for ws in bead_stores():
        name = ws.name
        reachable_ws = ws in listed or ws in pinned

        # R1 — a workspace no worker will ever scan.
        if not reachable_ws:
            beads = list_beads(ws)
            open_n = sum(1 for b in beads if b.get("status") == "open")
            if open_n:
                findings.append(Finding(
                    "R1_WORKSPACE_UNSCANNED", name, str(ws),
                    f"not in strands.explore.workspaces and pinned by no worker; "
                    f"{open_n} open beads are unreachable by any worker",
                    open_n,
                ))
            continue

        # R2 — a workspace whose adapter cannot be constructed. Its beads look
        # perfectly claimable and can never dispatch.
        adapter, source = workspace_adapter(ws, global_default, global_rules)
        if not adapter_exists(adapter):
            findings.append(Finding(
                "R2_ADAPTER_MISSING", name, adapter or "(unset)",
                f"resolved from {source}; no adapter YAML in {ADAPTER_DIR}. "
                f"Beads here claim successfully and can never dispatch",
            ))

        beads = list_beads(ws)

        # R3 — a claim held by an identity that does not exist. Invisible to
        # Mend (it filters on in_progress) and refused by `bead release`.
        stuck = [
            b for b in beads
            if b.get("status") == "open" and b.get("assignee")
            and not any(b["assignee"].endswith(ident) for ident in live)
        ]
        human = [b for b in stuck if not re.match(r"^(claude|codex|glm|c\d)", b["assignee"])]
        orphaned = [b for b in stuck if b not in human]
        if orphaned:
            who = Counter(b["assignee"] for b in orphaned)
            findings.append(Finding(
                "R3_ASSIGNEE_DEAD", name,
                ", ".join(f"{a} x{n}" for a, n in who.most_common(3)),
                f"{len(orphaned)} open beads assigned to identities with no live worker; "
                f"unclaimable, and no repair path reaches them",
                len(orphaned),
            ))
        if human:
            findings.append(Finding(
                "I_HUMAN_GATED", name,
                ", ".join(sorted({b["assignee"] for b in human})),
                f"{len(human)} open beads assigned to a non-worker identity "
                f"(deliberate gate — must NOT be auto-cleared)",
                len(human),
            ))

        # R4 — every non-closed bead excluded by label, forever.
        if exclude_labels:
            excluded = [
                b for b in beads
                if b.get("status") == "open"
                and exclude_labels & set(b.get("labels") or [])
            ]
            if excluded:
                findings.append(Finding(
                    "R4_LABEL_EXCLUDED", name,
                    ", ".join(sorted(exclude_labels & {l for b in excluded for l in b.get("labels") or []})),
                    f"{len(excluded)} open beads carry a pluck exclude_label and will never be claimed",
                    len(excluded),
                ))

        # R5 — claimed over and over, never dispatched.
        for bead_id, n in claim_churn(ws):
            findings.append(Finding(
                "R5_CLAIM_CHURN", name, bead_id,
                f"claimed {n}x in the last {int(CHURN_WINDOW.total_seconds()//3600)}h "
                f"with no dispatch; burning worker cycles",
                n,
            ))

        # R6 — a workspace with open beads where none is ready. Legitimate when
        # everything is blocked; a starvation signal when it is not.
        open_beads = [b for b in beads if b.get("status") == "open"]
        if open_beads:
            # Count lines, not "\nID:" -- the latter misses an ID: at offset 0,
            # so a workspace with exactly one ready bead reads as starved. That
            # off-by-one produced 11 false positives on the first run, which is
            # the failure mode this whole audit cannot afford: a detector that
            # cries wolf is a detector nobody reads.
            ready = sum(
                1 for line in subprocess.run(
                    ["bead", "list", "--ready", "--limit", "2000"],
                    cwd=ws, capture_output=True, text=True,
                ).stdout.splitlines()
                if line.startswith("ID:")
            )
            unblocked = [b for b in open_beads if not b.get("dependencies")]
            if ready == 0 and unblocked:
                findings.append(Finding(
                    "R6_FRONTIER_EMPTY", name, f"{len(open_beads)} open",
                    f"no ready beads despite {len(unblocked)} open beads with no dependencies",
                    len(unblocked),
                ))
    return findings


def main() -> int:
    try:
        findings = audit()
    except Exception as exc:  # audit failure must never read as "all clear"
        print(f"audit: FAILED: {exc}", file=sys.stderr)
        return 2

    if not findings:
        print("reachability audit: no unreachable work found")
        return 0

    informational = [f for f in findings if f.rule.startswith("I_")]
    violations = [f for f in findings if not f.rule.startswith("I_")]

    print(f"reachability audit: {len(violations)} violation(s), "
          f"{sum(f.count for f in violations)} bead(s) affected\n")
    for rule in sorted({f.rule for f in violations}):
        group = [f for f in violations if f.rule == rule]
        print(f"{rule}  ({sum(f.count for f in group)} beads)")
        for f in group:
            print(f"    {f.scope}: {f.subject}")
            print(f"        {f.detail}")
        print()
    if informational:
        print("informational (correct as-is, never auto-repair):")
        for f in informational:
            print(f"    {f.scope}: {f.subject} — {f.detail}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
