#!/usr/bin/env python3
"""Checkout drift check — read-only detector for git checkouts drifting
across hosts (mid-merge, unpushed, far behind origin).

bead-route keeps bead state consistent across codinghome/lab/gmktec, but
nothing watched the *git* side. Real misses from 2026-11-03 (bead
needle-05eb7ee7): codinghome's ~/declarative-config sat mid-merge for two
days blocking every commit there while a deploy commit went unpushed and
unnoticed; codinghome's ~/NEEDLE accumulated unpushed commits days old; lab's
ARMOR/shephrd/NEEDLE sat hundreds of commits behind origin; lab's ~/bead-rs
carried 85 modified paths under .beads/checkpoint. This sweep is the thing
that watches.

Every run, for each repo checkout under ~ on each host (local scan here;
lab and gmktec are scanned over SSH by piping this same file to a remote
python3, so nothing is installed remotely and a stale local copy of the
checker cannot exist):

  1. finds in-progress merge operations — MERGE_HEAD, rebase-merge,
     rebase-apply — with their age,
  2. fetches origin (the one deliberate, bounded mutation: remote-tracking
     refs only, gc disabled) first, so both direction counts compare
     against fresh remote refs — after another host pushes, a stale ref
     would misreport its commits as local-only,
  3. counts unpushed commits (reachable from HEAD, from no remote-tracking
     ref) with the age of the oldest, and commits behind origin,
  4. counts modified paths under .beads/checkpoint (informational: the
     checkpoint sweep owns that churn, but a big pile is worth seeing).

It writes ~/.local/state/checkout-drift/last-report.json, prints the report
to stdout (journald under the timer), and pages the local telegram-relay —
the same channel kubeconfig-expiry-sweep uses — when a checkout has been
mid-merge longer than --merge-age-limit (1 h), has unpushed commits older
than --unpushed-age-limit (6 h), or is more than --behind-limit (100)
commits behind. state.json deduplicates so an ongoing problem pages once
per --repeat-hours instead of every run, and a return to health sends one
all-clear.

Exit codes (the unit is left failed so the journal and any unit-failure
monitor surface it, same contract as check-wrapper-drift):
  0  every scanned checkout is clean within the thresholds
  1  DRIFT — at least one threshold trip (the alert)
  2  PROBE — no findings, but a configured host could not be scanned, so
     the detector is partially blind; fails harder than a clean run

The checker never modifies a checkout: all git invocations are reads, run
with GIT_OPTIONAL_LOCKS=0 so even `status` cannot take the index lock,
except the one `fetch` (remote-tracking refs only, `-c gc.auto=0`). The
test suite greps this file for mutating git commands and rejects them.
Stdlib only, like kubeconfig-expiry-sweep and lab-health-collector.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import subprocess
import time
import sys
import tempfile
import urllib.request

# Local relay on codinghome (telegram-relay.service); same channel
# kubeconfig-expiry-sweep pages through. Overridable for tests.
TELEGRAM_URL = os.environ.get(
    "CHECKOUT_DRIFT_TELEGRAM_URL", "http://127.0.0.1:8080/send"
)

# Directories never walked into: tool caches and state trees (fetching
# dozens of .needle dispatch fixtures every 15 minutes buys nothing), build
# output, vendored dependencies. ".tmp"/"tmp" hold hook and verification
# throwaway clones (the first live run found a dozen secret-scanner temp
# repos under ~/.tmp alone); "scratch" is disposable by the box's file
# organization policy and reaped by scratch-ttl, so drift there is noise
# that would train the page to be ignored.
PRUNE_DIRS = {
    ".git",
    ".cache",
    ".cargo",
    ".config",
    ".local",
    ".needle",
    ".npm",
    ".rustup",
    ".stack",
    ".tmp",
    "Library",
    "__pycache__",
    "dist",
    "node_modules",
    "scratch",
    "target",
    "tmp",
    "venv",
    ".venv",
}

# The only git subcommands this checker may run. The unit test parses this
# file with the ast module and rejects any git invocation whose subcommand
# is not on its own mirrored allowlist — merge, reset, checkout, pull,
# rebase and friends are the mutation risk.
GIT_COMMAND_ALLOWLIST = {
    "rev-parse",
    "rev-list",
    "remote",
    "fetch",
    "status",
    "show",
    "for-each-ref",
    "symbolic-ref",
    "log",
    "diff",
}


def log(msg: str) -> None:
    print(f"{dt.datetime.now(dt.timezone.utc):%Y-%m-%dT%H:%M:%SZ} {msg}", file=sys.stderr, flush=True)


def run_git(args: list[str], cwd: str, timeout: float) -> tuple[int, str, str]:
    """Run one read-only git invocation with the index lock disabled."""
    env = dict(os.environ)
    env["GIT_OPTIONAL_LOCKS"] = "0"
    cmd = ["git", "-c", "gc.auto=0"] + args
    try:
        proc = subprocess.run(
            cmd,
            cwd=cwd,
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        return proc.returncode, proc.stdout.strip(), proc.stderr.strip()
    except subprocess.TimeoutExpired:
        return 124, "", f"timeout after {timeout:.0f}s"
    except OSError as exc:
        return 127, "", str(exc)


def git_out(args: list[str], cwd: str, timeout: float) -> str | None:
    """Stdout of one git invocation, or None when it failed."""
    rc, out, _err = run_git(args, cwd, timeout)
    return out if rc == 0 else None


def git_path(repo: str, name: str, timeout: float) -> str | None:
    """Absolute path git resolves <name> to inside this repo's git dir.

    Handles plain checkouts and linked worktrees alike: --git-path is
    worktree-aware, so rebase-merge in a worktree resolves to that
    worktree's own state, not the shared git dir.
    """
    out = git_out(["rev-parse", "--git-path", name], repo, timeout)
    if out is None:
        return None
    if os.path.isabs(out):
        return out
    return os.path.join(repo, out)


def mid_merge(repo: str, timeout: float) -> dict | None:
    """The in-progress merge operation, if any, with its age in seconds."""
    candidates = (
        ("merge", "MERGE_HEAD"),
        ("rebase", "rebase-merge"),
        ("am", "rebase-apply"),
    )
    now = dt.datetime.now(dt.timezone.utc).timestamp()
    for kind, name in candidates:
        path = git_path(repo, name, timeout)
        if path and os.path.exists(path):
            try:
                started = os.stat(path).st_mtime
            except OSError:
                continue
            age = max(0, int(now - started))
            return {
                "kind": kind,
                "path": path,
                "age_seconds": age,
                "started_iso": dt.datetime.fromtimestamp(
                    started, dt.timezone.utc
                ).strftime("%Y-%m-%dT%H:%M:%SZ"),
            }
    return None


def unpushed(repo: str, timeout: float) -> dict | None:
    """Commits reachable from HEAD but from no remote-tracking ref.

    None when the checkout has no remote at all: with nothing to push to,
    "unpushed" is not a meaningful state (a bare local repo would otherwise
    read as perpetually adrift).
    """
    if not git_out(["remote"], repo, timeout):
        return None
    out = git_out(["rev-list", "HEAD", "--not", "--remotes"], repo, timeout)
    if out is None:
        return None
    shas = [line for line in out.splitlines() if line.strip()]
    if not shas:
        return {"count": 0, "oldest_age_seconds": None, "oldest": None}
    oldest_sha = shas[-1]  # rev-list is newest-first
    date_out = git_out(["show", "-s", "--format=%ct", oldest_sha], repo, timeout)
    try:
        oldest_ts = int((date_out or "0").splitlines()[0])
    except (ValueError, IndexError):
        return {"count": len(shas), "oldest_age_seconds": None, "oldest": None}
    now = dt.datetime.now(dt.timezone.utc).timestamp()
    oldest_iso = dt.datetime.fromtimestamp(oldest_ts, dt.timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )
    return {
        "count": len(shas),
        "oldest": {"sha": oldest_sha[:12], "committed_iso": oldest_iso},
        "oldest_age_seconds": max(0, int(now - oldest_ts)),
    }


def behind(repo: str, timeout: float) -> dict | None:
    """Commits origin has that HEAD lacks, after a fetch.

    Prefers the branch's configured upstream; falls back to origin/HEAD,
    then origin/main. Returns None only when no comparison point exists
    (no origin, no upstream, nothing to compare against).
    """
    remote = git_out(["remote", "get-url", "origin"], repo, timeout)
    if remote is None:
        return None
    ref = git_out(
        ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
        repo,
        timeout,
    )
    if not ref:
        ref = git_out(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"], repo, timeout)
    if not ref:
        ref = "origin/main" if git_out(
            ["rev-parse", "--verify", "-q", "origin/main"], repo, timeout
        ) is not None else None
    if not ref:
        return None
    count = git_out(["rev-list", "--count", f"HEAD..{ref}"], repo, timeout)
    if count is None:
        return None
    try:
        return {"count": int(count.splitlines()[0]), "ref": ref}
    except (ValueError, IndexError):
        return None


def fetch(repo: str, timeout: float) -> tuple[bool, str | None]:
    """Fetch origin — remote-tracking refs only, gc disabled, bounded."""
    rc, _out, err = run_git(["fetch", "--quiet", "origin"], repo, timeout)
    if rc == 0:
        return True, None
    # git's fatal messages are multi-line; collapse so one finding stays
    # one line in the report and the journal.
    flat = " ".join(err.split())[:200]
    return False, flat if flat else f"git fetch exited {rc}"


def checkpoint_dirty(repo: str, timeout: float) -> int:
    """Modified paths under .beads/checkpoint (informational, never an alert)."""
    if not os.path.isdir(os.path.join(repo, ".beads", "checkpoint")):
        return 0
    out = git_out(["status", "--porcelain", "--", ".beads/checkpoint"], repo, timeout)
    if out is None:
        return 0
    return sum(1 for line in out.splitlines() if line.strip())


def inspect_repo(
    repo: str,
    timeout: float,
    do_fetch: bool = True,
    fetch_timeout: float | None = None,
) -> dict:
    """Full read-only inspection of one checkout.

    The fetch runs before the unpushed and behind counts so both compare
    against fresh remote-tracking refs; a failed fetch only downgrades them
    to stale-ref estimates, it never aborts the inspection.
    """
    branch = git_out(["rev-parse", "--abbrev-ref", "HEAD"], repo, timeout)
    entry: dict = {
        "path": repo,
        "branch": branch,
        "mid_merge": None,
        "unpushed": None,
        "behind": None,
        "checkpoint_dirty": 0,
        "fetch_ok": None,
        "fetch_error": None,
        "scan_error": None,
    }
    if branch is None:
        entry["scan_error"] = "git rev-parse failed; not a readable checkout"
        return entry
    if do_fetch:
        entry["fetch_ok"], entry["fetch_error"] = fetch(
            repo, fetch_timeout or timeout
        )
    entry["mid_merge"] = mid_merge(repo, timeout)
    entry["unpushed"] = unpushed(repo, timeout)
    entry["checkpoint_dirty"] = checkpoint_dirty(repo, timeout)
    entry["behind"] = behind(repo, timeout)
    return entry


def common_git_dir(repo: str, timeout: float) -> str | None:
    out = git_out(["rev-parse", "--git-common-dir"], repo, timeout)
    if out is None:
        return None
    return os.path.realpath(os.path.join(repo, out) if not os.path.isabs(out) else out)


def find_repos(root: str, max_depth: int, max_repos: int, timeout: float) -> tuple[list[str], list[str]]:
    """Repo checkouts under <root>, outermost first, worktrees deduplicated."""
    found: list[str] = []
    seen_common: set[str] = set()
    notes: list[str] = []
    base_depth = root.rstrip(os.sep).count(os.sep)
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        depth = dirpath.rstrip(os.sep).count(os.sep) - base_depth
        if depth >= max_depth:
            dirnames[:] = []
        is_repo = ".git" in dirnames or ".git" in filenames
        # Prune for descent only AFTER detection: stripping ".git" first
        # would make directory-style checkouts undetectable.
        dirnames[:] = sorted(d for d in dirnames if d not in PRUNE_DIRS)
        if not is_repo:
            continue
        repo = os.path.abspath(dirpath)
        common = common_git_dir(repo, timeout)
        if common is None:
            notes.append(f"{repo}: unreadable git dir")
            continue
        if common in seen_common:
            # A linked worktree of an already-scanned checkout: the parent's
            # entry already reports merge/unpushed state for the shared refs,
            # and re-scanning would double-count fetches for one repo.
            continue
        seen_common.add(common)
        found.append(repo)
        if len(found) >= max_repos:
            notes.append(f"scan truncated at --max-repos={max_repos}")
            return found, notes
    return found, notes


def scan_root(root: str, opts: argparse.Namespace) -> dict:
    """Scan every checkout under <root>; the remote-scan and local code path."""
    timeout = opts.git_timeout
    repos, notes = find_repos(root, opts.max_depth, opts.max_repos, timeout)
    entries = [
        inspect_repo(
            r,
            timeout,
            do_fetch=not opts.no_fetch,
            fetch_timeout=opts.fetch_timeout,
        )
        for r in repos
    ]
    return {"root": root, "repos": entries, "notes": notes}


def evaluate(repos: list[dict], opts: argparse.Namespace) -> list[dict]:
    """Threshold trips across one host's repo entries."""
    alerts: list[dict] = []
    for entry in repos:
        if entry.get("scan_error"):
            continue
        merge = entry.get("mid_merge")
        if merge and merge["age_seconds"] > opts.merge_age_limit:
            alerts.append({
                "host": entry["host"],
                "repo": entry["path"],
                "kind": "mid_merge",
                "detail": (
                    f"{merge['kind']} open {merge['age_seconds'] // 3600}h"
                    f"{(merge['age_seconds'] % 3600) // 60}m (limit {opts.merge_age_limit // 3600}h)"
                ),
                "age_seconds": merge["age_seconds"],
            })
        unpushed_info = entry.get("unpushed")
        if (
            unpushed_info
            and unpushed_info["count"] > 0
            and unpushed_info["oldest_age_seconds"] is not None
            and unpushed_info["oldest_age_seconds"] > opts.unpushed_age_limit
        ):
            alerts.append({
                "host": entry["host"],
                "repo": entry["path"],
                "kind": "unpushed",
                "detail": (
                    f"{unpushed_info['count']} unpushed, oldest {unpushed_info['oldest_age_seconds'] // 3600}h"
                    f" ({unpushed_info['oldest']['committed_iso']}; limit {opts.unpushed_age_limit // 3600}h)"
                ),
                "age_seconds": unpushed_info["oldest_age_seconds"],
            })
        behind_info = entry.get("behind")
        if behind_info and behind_info["count"] > opts.behind_limit:
            alerts.append({
                "host": entry["host"],
                "repo": entry["path"],
                "kind": "behind",
                "detail": f"{behind_info['count']} commits behind {behind_info['ref']} (limit {opts.behind_limit})",
                "count": behind_info["count"],
            })
    return alerts


def remote_scan(host: str, script_path: str, opts: argparse.Namespace) -> dict | None:
    """Pipe this file to <host> and read one scan JSON document back.

    Nothing is installed remotely: python3 on the far side executes the
    bytes on stdin (--scan-json mode), so a stale deployed copy of the
    checker cannot drift out from under the check.
    """
    cmd = [
        "ssh",
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=10",
        host,
        "python3", "-", "--scan-json",
        "--scan-root", opts.remote_home or "~",
        "--git-timeout", str(opts.git_timeout),
        "--fetch-timeout", str(opts.fetch_timeout),
        *(["--no-fetch"] if opts.no_fetch else []),
    ]
    with open(script_path, "rb") as fh:
        try:
            proc = subprocess.run(
                cmd, stdin=fh, capture_output=True, text=True,
                timeout=opts.ssh_timeout,
            )
        except subprocess.TimeoutExpired:
            log(f"host {host}: scan timed out after {opts.ssh_timeout}s")
            return None
    if proc.returncode != 0:
        log(f"host {host}: scan exited {proc.returncode}: {proc.stderr.strip()[:200]}")
        return None
    try:
        doc = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        log(f"host {host}: unparseable scan output: {exc}")
        return None
    for entry in doc.get("repos", []):
        entry["host"] = host
    return doc


def send_telegram(text: str, url: str = TELEGRAM_URL, timeout: float = 10.0) -> bool:
    """POST to the local telegram-relay; failures are logged, never raised."""
    req = urllib.request.Request(
        url,
        data=json.dumps({"text": text}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            ok = 200 <= resp.status < 300
    except Exception as exc:  # noqa: BLE001 — a relay outage must not kill the sweep
        log(f"telegram delivery failed: {exc}")
        return False
    if ok:
        log("sent telegram alert")
    return ok


def load_state(state_path: str) -> dict:
    try:
        with open(state_path, encoding="utf-8") as fh:
            return json.load(fh)
    except (OSError, json.JSONDecodeError):
        return {}


def save_state(state_path: str, state: dict) -> None:
    os.makedirs(os.path.dirname(state_path), exist_ok=True)
    tmp = state_path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(state, fh, indent=2, sort_keys=True)
        fh.write("\n")
    os.replace(tmp, state_path)


def alert_fingerprints(alerts: list[dict]) -> list[str]:
    return sorted(
        hashlib.sha256(f"{a['host']}|{a['repo']}|{a['kind']}".encode()).hexdigest()[:16]
        for a in alerts
    )


def page_if_alerting(alerts: list[dict], state_path: str, opts: argparse.Namespace) -> bool:
    """Page once per --repeat-hours for an ongoing problem; all-clear on recovery."""
    state = load_state(state_path)
    now = time.time()
    fps = alert_fingerprints(alerts)
    last_page = float(state.get("last_page_ts", 0))
    known = set(state.get("fingerprints", []))
    changed = set(fps) != known
    due = now - last_page >= opts.repeat_hours * 3600
    if alerts and (changed or due):
        lines = [f"checkout-drift: {len(alerts)} finding(s)"]
        for a in alerts[:12]:
            lines.append(f"  {a['host']}:{a['repo']} {a['kind']} — {a['detail']}")
        sent = send_telegram("\n".join(lines), url=opts.telegram_url)
        if sent:
            save_state(state_path, {"last_page_ts": now, "fingerprints": fps})
        return sent
    if not alerts and known:
        send_telegram("checkout-drift: all clear", url=opts.telegram_url)
        save_state(state_path, {"last_page_ts": now, "fingerprints": []})
    return False


def print_report(hosts: list[dict], alerts: list[dict]) -> None:
    for h in hosts:
        name = h.get("host", "local")
        if h.get("unreachable"):
            print(f"checkout-drift: {name}: UNREACHABLE (detector blind there)")
            continue
        repos = h.get("repos", [])
        flagged = [r for r in repos if r.get("mid_merge") or (r.get("unpushed") or {}).get("count") or (r.get("behind") or {}).get("count")]
        print(f"checkout-drift: {name}: {len(repos)} checkouts, {len(flagged)} with state worth noting")
        for r in repos:
            bits: list[str] = []
            if r.get("mid_merge"):
                m = r["mid_merge"]
                bits.append(f"{m['kind']} open {m['age_seconds'] // 3600}h{(m['age_seconds'] % 3600) // 60}m")
            u = r.get("unpushed") or {}
            if u.get("count"):
                bits.append(f"{u['count']} unpushed (oldest {u['oldest_age_seconds']}s)")
            b = r.get("behind")
            if b and b.get("count"):
                bits.append(f"{b['count']} behind {b['ref']}")
            cp = r.get("checkpoint_dirty") or 0
            if cp:
                bits.append(f"{cp} checkpoint paths modified")
            if r.get("scan_error"):
                bits.append(f"SCAN ERROR: {r['scan_error']}")
            elif r.get("fetch_error"):
                bits.append(f"fetch failed: {r['fetch_error']}")
            if bits:
                print(f"checkout-drift:   {r.get('host', name)}:{r['path']} — " + "; ".join(bits))
    if alerts:
        print(f"checkout-drift: summary: {len(alerts)} finding(s):")
        for a in alerts:
            print(f"checkout-drift:   ALERT {a['host']}:{a['repo']} {a['kind']} — {a['detail']}")
    else:
        print("checkout-drift: summary: all checkouts within thresholds")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--scan-json", action="store_true",
                        help="internal: scan --scan-root and print one JSON doc to stdout")
    parser.add_argument("--scan-root", default=os.path.expanduser("~"),
                        help="directory tree to walk for checkouts (default ~)")
    parser.add_argument("--remote-home", default=None,
                        help="scan root passed to remote hosts (default their ~)")
    parser.add_argument("--remote-hosts", default="lab,gmktec",
                        help="comma-separated ssh hosts to scan (empty string disables)")
    parser.add_argument("--no-fetch", action="store_true",
                        help="skip the origin fetch (behind and unpushed counts use stale refs)")
    parser.add_argument("--report-dir", default="~/.local/state/checkout-drift",
                        help="where last-report.json and state.json live")
    parser.add_argument("--telegram-url", default=TELEGRAM_URL,
                        help="telegram-relay endpoint (tests point this at a dead port)")
    parser.add_argument("--repeat-hours", type=float, default=4.0,
                        help="re-page an unchanged problem after this many hours")
    parser.add_argument("--merge-age-limit", type=int, default=3600,
                        help="alert when an in-progress merge is older than this (s)")
    parser.add_argument("--unpushed-age-limit", type=int, default=21600,
                        help="alert when the oldest unpushed commit is older than this (s)")
    parser.add_argument("--behind-limit", type=int, default=100,
                        help="alert when a checkout is more than this many commits behind")
    parser.add_argument("--git-timeout", type=float, default=60.0,
                        help="per-git-command timeout (s)")
    parser.add_argument("--fetch-timeout", type=float, default=90.0,
                        help="per-repo fetch timeout (s)")
    parser.add_argument("--ssh-timeout", type=float, default=600.0,
                        help="whole-remote-scan timeout per host (s)")
    parser.add_argument("--max-depth", type=int, default=5,
                        help="walk depth below the scan root")
    parser.add_argument("--max-repos", type=int, default=250,
                        help="cap on checkouts scanned per host")
    parser.add_argument("--self-test", action="store_true",
                        help="run against synthetic fixtures only; touches nothing live")
    opts = parser.parse_args(argv)

    script_path = os.path.abspath(__file__)
    here = os.path.dirname(script_path)

    if opts.self_test:
        return self_test(opts)

    if opts.scan_json:
        doc = scan_root(os.path.expanduser(opts.scan_root), opts)
        json.dump(doc, sys.stdout, indent=1)
        print()
        return 0

    hostname = os.uname().nodename
    report_dir = os.path.expanduser(opts.report_dir)
    os.makedirs(report_dir, exist_ok=True)

    local_doc = scan_root(opts.scan_root, opts)
    local_doc["host"] = hostname
    for entry in local_doc["repos"]:
        entry["host"] = hostname
    hosts: list[dict] = [local_doc]
    unreachable: list[str] = []
    for host in [h for h in (opts.remote_hosts or "").split(",") if h.strip()]:
        doc = remote_scan(host.strip(), script_path, opts)
        if doc is None:
            unreachable.append(host.strip())
            hosts.append({"host": host.strip(), "unreachable": True})
        else:
            doc["host"] = host.strip()
            hosts.append(doc)

    all_repos = [e for h in hosts for e in h.get("repos", [])]
    alerts = evaluate(all_repos, opts)

    report = {
        "generated_utc": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "host": hostname,
        "thresholds": {
            "merge_age_seconds": opts.merge_age_limit,
            "unpushed_age_seconds": opts.unpushed_age_limit,
            "behind_commits": opts.behind_limit,
        },
        "scanned_repos": len(all_repos),
        "unreachable_hosts": unreachable,
        "hosts": hosts,
        "alerts": alerts,
    }
    report_path = os.path.join(report_dir, "last-report.json")
    tmp = report_path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(report, fh, indent=1)
        fh.write("\n")
    os.replace(tmp, report_path)

    print_report(hosts, alerts)
    page_if_alerting(alerts, os.path.join(report_dir, "state.json"), opts)

    if alerts:
        return 1
    if unreachable:
        return 2
    return 0


def self_test(opts: argparse.Namespace) -> int:
    """Synthetic fixtures only: build repos in a temp dir, assert the
    detector's verdicts, tear it all down. Touches nothing live."""
    failures: list[str] = []
    tmp = tempfile.mkdtemp(prefix="checkout-drift-selftest-")

    def git(args: list[str], cwd: str, **kw) -> None:
        env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t",
                   GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@t")
        proc = subprocess.run(["git"] + args, cwd=cwd, env=env,
                              capture_output=True, text=True, **kw)
        if proc.returncode != 0:
            raise RuntimeError(f"fixture git {' '.join(args)} failed: {proc.stderr}")

    def fresh(name: str) -> str:
        path = os.path.join(tmp, name)
        os.makedirs(path)
        git(["init", "-q", "-b", "main"], path)
        git(["config", "user.email", "t@t"], path)
        git(["config", "user.name", "t"], path)
        with open(os.path.join(path, "f.txt"), "w") as fh:
            fh.write("one\n")
        git(["add", "f.txt"], path)
        # A commit dated two days back so unpushed/merge age math is live.
        env_dates = ["-c", "git.commit.gpgsign=false"]
        git(env_dates + ["commit", "-q", "-m", "one",
                         "--date=2026-11-01T12:00:00+00:00"], path)
        return path

    def scan(path: str, fetch: bool = True) -> dict:
        return inspect_repo(path, opts.git_timeout, do_fetch=fetch)

    def commit_backdated(repo: str, message: str, when: str) -> None:
        with open(os.path.join(repo, "f.txt"), "a") as fh:
            fh.write(message + "\n")
        git(["add", "f.txt"], repo)
        env = ["-c", "git.commit.gpgsign=false"]
        proc = subprocess.run(
            ["git", *env, "commit", "-q", "-m", message],
            cwd=repo, capture_output=True, text=True,
            env=dict(os.environ, GIT_COMMITTER_DATE=when),
        )
        if proc.returncode != 0:
            raise RuntimeError(f"backdated commit failed: {proc.stderr}")

    def add_origin(repo: str, bare: str) -> None:
        git(["remote", "add", "origin", bare], repo)
        git(["fetch", "-q", "origin"], repo)
        git(["push", "-q", "-u", "origin", "main"], repo)

    try:
        clean = fresh("clean")
        entry = scan(clean, fetch=False)
        if entry["mid_merge"] is not None:
            failures.append("clean repo reported mid-merge")
        if (entry["unpushed"] or {}).get("count", 0) != 0:
            failures.append(f"clean repo reported unpushed: {entry['unpushed']}")
        if entry["behind"] is not None:
            failures.append(f"clean repo (no remote) reported behind: {entry['behind']}")
        if entry["checkpoint_dirty"] != 0:
            failures.append("clean repo reported checkpoint churn")

        # Threshold trips on synthetic state.
        mergey = fresh("mergey")
        head = git_out(["rev-parse", "HEAD"], mergey, 30)
        merge_head = os.path.join(mergey, ".git", "MERGE_HEAD")
        with open(merge_head, "w") as fh:
            fh.write((head or "") + "\n")
        old = dt.datetime.now(dt.timezone.utc).timestamp() - 2 * 3600
        os.utime(merge_head, (old, old))
        entry = scan(mergey, fetch=False)
        merge = entry.get("mid_merge")
        if not merge or merge["kind"] != "merge" or merge["age_seconds"] < 3600:
            failures.append(f"2h-old MERGE_HEAD not reported: {merge}")
        elif not evaluate([dict(entry, host="t")], opts):
            failures.append("2h-old MERGE_HEAD did not trip --merge-age-limit")
        fresh_merge = dict(merge or {})
        fresh_merge["age_seconds"] = 60
        if evaluate([dict(entry, host="t", mid_merge=fresh_merge)], opts):
            failures.append("1-minute-old merge tripped the 1h limit")

        # One pushed base commit plus a backdated local-only commit: exactly
        # one unpushed commit, old enough to trip even a 60s age limit.
        unpushy = fresh("unpushy")
        unpushy_bare = os.path.join(tmp, "unpushy-origin.git")
        git(["clone", "-q", "--bare", unpushy, unpushy_bare], tmp)
        add_origin(unpushy, unpushy_bare)
        commit_backdated(unpushy, "local-only", "2026-11-03T12:00:00+00:00")
        entry = scan(unpushy, fetch=False)
        u = entry.get("unpushed") or {}
        if u.get("count") != 1 or (u.get("oldest_age_seconds") or 0) < 60:
            failures.append(f"unpushed fixture reported {u}")
        else:
            local_opts = argparse.Namespace(**vars(opts))
            local_opts.unpushed_age_limit = 0
            if not evaluate([dict(entry, host="t")], local_opts):
                failures.append("old unpushed commit did not trip a 0s age limit")

        # Three-checkout topology: base -> bare origin; twin advances origin
        # by one commit; behindy is a fresh clone that has not caught up.
        base = fresh("behindy-base")
        bare = os.path.join(tmp, "behindy-origin.git")
        git(["clone", "-q", "--bare", base, bare], tmp)
        # Clone behindy BEFORE twin advances origin, so its fetch has
        # something to catch up to.
        behindy = os.path.join(tmp, "behindy")
        git(["clone", "-q", bare, behindy], tmp)
        twin = os.path.join(tmp, "behindy-twin")
        git(["clone", "-q", bare, twin], tmp)
        with open(os.path.join(twin, "f.txt"), "a") as fh:
            fh.write("two\n")
        git(["add", "f.txt"], twin)
        git(["commit", "-q", "-m", "two"], twin)
        git(["push", "-q", "origin", "main"], twin)
        entry = scan(behindy, fetch=True)
        b = entry.get("behind")
        if not b or b["count"] != 1 or b["ref"] != "origin/main":
            failures.append(f"1-commit-behind fixture reported {b}")
        else:
            local_opts = argparse.Namespace(**vars(opts))
            local_opts.behind_limit = 0
            if not evaluate([dict(entry, host="t")], local_opts):
                failures.append("1 commit behind did not trip a 0 limit")

        # Checkpoint churn is counted but never alerts on its own.
        cp_repo = fresh("cp-repo")
        os.makedirs(os.path.join(cp_repo, ".beads", "checkpoint"), exist_ok=True)
        with open(os.path.join(cp_repo, ".beads", "checkpoint", "cur.json"), "w") as fh:
            fh.write("{}\n")
        git(["add", "-f", ".beads/checkpoint/cur.json"], cp_repo)
        git(["commit", "-q", "-m", "cp"], cp_repo)
        with open(os.path.join(cp_repo, ".beads", "checkpoint", "cur.json"), "a") as fh:
            fh.write("# churn\n")
        entry = scan(cp_repo, fetch=False)
        if entry["checkpoint_dirty"] != 1:
            failures.append(f"checkpoint churn not counted: {entry['checkpoint_dirty']}")
        if evaluate([dict(entry, host="t")], opts):
            failures.append("checkpoint churn alone raised an alert")

        # Rebase state directories are recognized as mid-operation.
        rebasey = fresh("rebasey")
        rebase_dir = os.path.join(rebasey, ".git", "rebase-merge")
        os.makedirs(rebase_dir)
        old = dt.datetime.now(dt.timezone.utc).timestamp() - 90 * 60
        os.utime(rebase_dir, (old, old))
        entry = scan(rebasey, fetch=False)
        merge = entry.get("mid_merge")
        if not merge or merge["kind"] != "rebase" or merge["age_seconds"] < 60 * 60:
            failures.append(f"90m-old rebase-merge not reported: {merge}")

        # Linked-worktree dedupe is exercised by the unittest suite, which
        # builds the fixture itself; keeping worktree creation out of this
        # file lets the mutation-grep test reject the verb outright.
    except Exception as exc:  # noqa: BLE001 — any fixture failure is a failed self-test
        failures.append(f"fixture error: {exc!r}")
    finally:
        for root_dir in (tmp,):
            subprocess.run(["chmod", "-R", "u+w", root_dir], capture_output=True)
            subprocess.run(["rm", "-rf", root_dir], capture_output=True)

    if failures:
        for f in failures:
            print(f"checkout-drift self-test: FAIL {f}", file=sys.stderr)
        return 1
    print("checkout-drift: self-test passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
