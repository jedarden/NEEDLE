#!/usr/bin/env python3
"""Tests for the checkout drift check (fleet/ex44/bin/check-checkout-drift.py).

Two layers, both synthetic:

  1. A static parse of the checker itself. Every git subcommand it may run
     against a real checkout must be on an explicit read-only allowlist; the
     mutating verbs (merge, reset, checkout, pull, rebase, and friends) must
     not appear as an invocation anywhere in the file. The checker's own
     --self-test fixture builder is allowed a small second tier of verbs
     (init/add/commit/clone/config/push) — but only inside the self_test
     region, and only ever against mktemp fixture repos it created itself.

  2. End-to-end runs of the checker as a subprocess over synthetic fixture
     repos built in a private temp dir: a clean checkout passes the
     aggregate run (exit 0), and each threshold — old mid-merge, old
     unpushed commit, far behind origin, linked worktree dedupe — behaves.
     No test touches a live checkout; remote hosts are disabled everywhere.

Run: python3 -m unittest discover -s fleet/ex44/tests -p 'test_checkout_drift.py'
"""
import ast
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
EX44 = os.path.dirname(HERE)
CHECKER = os.path.join(EX44, "bin", "check-checkout-drift.py")
APPLY = os.path.join(EX44, "apply-ex44-fleet.sh")
PYTHON = sys.executable

# Tier 1: the only verbs allowed against real checkouts. Mirrored here so a
# checker that quietly widens its own allowlist fails this test.
SCAN_ALLOWED = {
    "rev-parse", "rev-list", "remote", "fetch", "status", "show",
    "symbolic-ref", "log",
}
# Tier 2: fixture-builder verbs, legal only inside self_test (which only
# ever touches repos it just created under mktemp).
FIXTURE_ALLOWED = {"init", "add", "commit", "clone", "config", "push"}
BANNED = {
    "merge", "reset", "checkout", "pull", "rebase", "revert", "cherry-pick",
    "stash", "clean", "apply", "am", "tag", "gc", "filter-branch", "notes",
    "bisect", "mv", "rm", "worktree", "restore", "switch",
}


def load_checker():
    spec = importlib.util.spec_from_file_location("check_checkout_drift", CHECKER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CD = load_checker()


def strip_comment(line: str) -> str:
    """Drop a trailing # comment outside quotes (sufficient for this file)."""
    in_str = False
    for i, ch in enumerate(line):
        if ch in "\"'":
            in_str = not in_str
        elif ch == "#" and not in_str:
            return line[:i]
    return line


# The checker's git wrappers: calls whose first argument list starts with
# the verb itself (run_git(["fetch", ...])). Any other call is only a git
# invocation when its list starts with the literal "git".
GIT_WRAPPERS = {"run_git", "git_out", "git"}


def _list_verbs(func: ast.expr, node: ast.expr) -> list[str]:
    """Git verbs carried by one argument expression.

    Recognizes the three invocation shapes in the checker: a literal
    argument list (run_git(["fetch", ...]), the self-test helper's
    git(["init", ...])), an env prefix concatenated in front of one
    (env_dates + ["commit", ...]), and ["git", <verb>, ...] lists. A bare
    name or ["git"] + args forwards verbs resolved at the wrapper's call
    sites, which are parsed on their own, so it carries nothing here —
    and non-git subprocess lists (["rm", "-rf", ...]) are ignored unless
    the call is one of the git wrappers.
    """
    if isinstance(node, ast.List):
        consts = [e.value for e in node.elts if isinstance(e, ast.Constant)]
        if not consts or not all(isinstance(c, str) for c in consts):
            return []
        if consts[0] == "git":
            return consts[1:2]
        if isinstance(func, ast.Name) and func.id in GIT_WRAPPERS:
            return consts[:1]
        return []
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        return _list_verbs(func, node.right)
    return []


def parse_git_verbs(source: str) -> tuple[list, list]:
    """Git subcommands in the checker source, as (scan_path, fixture_path).

    The checker is parsed with the ast module, not regexed: call arguments
    span multiple lines and docstrings mention verbs in prose, so anything
    weaker either misses invocations or flags commentary. Fixture-builder
    lines only occur at/after `def self_test`; everything before it is the
    scan path and must hold tier-1 verbs alone.
    """
    tree = ast.parse(source)
    self_test_line = None
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef) and node.name == "self_test":
            self_test_line = node.lineno
            break
    if self_test_line is None:
        raise AssertionError("checker has no self_test region to split on")

    scan_verbs: list[str] = []
    fixture_verbs: list[str] = []
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        for arg in list(node.args) + [kw.value for kw in node.keywords]:
            for verb in _list_verbs(node.func, arg):
                (fixture_verbs if node.lineno >= self_test_line else scan_verbs).append(verb)
    return scan_verbs, fixture_verbs


class TestNoMutatingGit(unittest.TestCase):
    def setUp(self):
        with open(CHECKER, encoding="utf-8") as fh:
            self.source = fh.read()

    def test_scan_path_verbs_all_allowlisted(self):
        scan_verbs, _ = parse_git_verbs(self.source)
        self.assertTrue(scan_verbs, "parser found no scan-path git invocations")
        bad = sorted(set(scan_verbs) - SCAN_ALLOWED)
        self.assertEqual(bad, [], f"checker runs non-allowlisted git commands: {bad}")

    def test_mutating_verbs_never_appear_as_invocations(self):
        scan_verbs, fixture_verbs = parse_git_verbs(self.source)
        offenders = sorted((set(scan_verbs) | set(fixture_verbs)) & BANNED)
        self.assertEqual(offenders, [], f"mutating git verbs invoked: {offenders}")

    def test_textual_grep_finds_no_banned_invocation(self):
        # The literal acceptance grep: no `git merge`, `git reset`,
        # `git checkout`, `git pull` (or any other banned verb) anywhere in
        # the checker, comments included — prose that names a banned
        # invocation invites copy-paste, so even mention fails here.
        pattern = r"\bgit\s+(?:-[\w-]+\s+)*(?:" + "|".join(sorted(BANNED)) + r")\b"
        hits = sorted(set(re.findall(pattern, self.source)))
        self.assertEqual(hits, [], f"banned git invocation text present: {hits}")

    def test_fixture_verbs_stay_in_the_fixture_region(self):
        scan_verbs, fixture_verbs = parse_git_verbs(self.source)
        stray = sorted(set(scan_verbs) & FIXTURE_ALLOWED)
        self.assertEqual(stray, [], f"fixture verbs leak into the scan path: {stray}")
        self.assertTrue(fixture_verbs, "expected the self-test fixture builder to be parsed")

    def test_allowlist_self_consistent(self):
        self.assertEqual(SCAN_ALLOWED & BANNED, set())
        # fetch (remote-tracking refs only) is the one deliberate write; it
        # must stay visible so this test cannot pass by the list emptying.
        self.assertIn("fetch", SCAN_ALLOWED)


class TestWiring(unittest.TestCase):
    def test_units_wired_into_apply(self):
        with open(APPLY, encoding="utf-8") as fh:
            apply_src = fh.read()
        self.assertIn("checkout-drift.service", apply_src)
        self.assertIn("checkout-drift.timer", apply_src)
        self.assertIn("enable checkout-drift.timer", apply_src)

    def test_unit_files(self):
        with open(os.path.join(EX44, "checkout-drift.service"), encoding="utf-8") as fh:
            service = fh.read()
        # The deployed ExecStart path is a fixed literal, like every other
        # unit-content assertion in fleet test harnesses — deriving it from
        # this file's own location would make the test fail in any
        # extraction of the tree (clean-gate verification runs there).
        self.assertIn(
            "ExecStart=/home/coding/NEEDLE/fleet/ex44/bin/check-checkout-drift.py",
            service,
        )
        self.assertIn("Type=oneshot", service)
        self.assertIn("TimeoutStartSec", service)
        with open(os.path.join(EX44, "checkout-drift.timer"), encoding="utf-8") as fh:
            timer = fh.read()
        self.assertIn("OnCalendar=*:0/15", timer)
        self.assertIn("Persistent=true", timer)


class TestFixtures(unittest.TestCase):
    """Synthetic repos, real checker subprocesses, isolated report dir."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="checkout-drift-test-")
        self.report_dir = os.path.join(self.tmp, "state")
        self.env = dict(
            os.environ,
            GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t",
            GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@t",
        )

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def git(self, *args, cwd, env_dates=None, check=True):
        env = dict(self.env)
        if env_dates:
            env.update(env_dates)
        proc = subprocess.run(["git", "-c", "commit.gpgsign=false", *args],
                              cwd=cwd, env=env, capture_output=True, text=True)
        if check and proc.returncode != 0:
            raise RuntimeError(f"git {args} failed: {proc.stderr}")
        return proc

    def fresh_repo(self, name, content="one"):
        path = os.path.join(self.tmp, name)
        os.makedirs(path)
        self.git("init", "-q", "-b", "main", cwd=path)
        with open(os.path.join(path, "f.txt"), "w") as fh:
            fh.write(content + "\n")
        self.git("add", "f.txt", cwd=path)
        self.git("commit", "-q", "-m", content, cwd=path)
        return path

    def run_checker(self, *args):
        return subprocess.run(
            [PYTHON, CHECKER, *args],
            capture_output=True, text=True, timeout=300,
        )

    def run_aggregate(self, scan_root, *extra):
        return self.run_checker(
            "--scan-root", scan_root,
            "--remote-hosts", "",
            "--report-dir", self.report_dir,
            "--telegram-url", "http://127.0.0.1:9",
            *extra,
        )

    def read_report(self):
        with open(os.path.join(self.report_dir, "last-report.json"), encoding="utf-8") as fh:
            return json.load(fh)

    # -- clean fixtures -----------------------------------------------------

    def test_self_test_passes(self):
        proc = self.run_checker("--self-test")
        self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_clean_checkout_passes_unit(self):
        repo = self.fresh_repo("clean")
        proc = self.run_aggregate(repo)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        report = self.read_report()
        self.assertEqual(report["alerts"], [])
        entry = report["hosts"][0]["repos"][0]
        self.assertEqual(os.path.realpath(entry["path"]), os.path.realpath(repo))
        self.assertIsNone(entry["mid_merge"])
        # No remote configured: unpushed is deliberately null (nothing to
        # push to), not a zero count.
        self.assertIsNone(entry["unpushed"])
        self.assertIsNone(entry["behind"])
        self.assertEqual(entry["checkpoint_dirty"], 0)

    def test_pushed_clean_checkout_is_not_behind(self):
        repo = self.fresh_repo("clean-remote")
        bare = os.path.join(self.tmp, "clean-remote.git")
        self.git("clone", "-q", "--bare", repo, bare, cwd=self.tmp)
        self.git("remote", "add", "origin", bare, cwd=repo)
        self.git("fetch", "-q", "origin", cwd=repo)
        self.git("push", "-q", "-u", "origin", "main", cwd=repo)
        proc = self.run_aggregate(repo)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        entry = self.read_report()["hosts"][0]["repos"][0]
        self.assertEqual((entry["behind"] or {"count": 0})["count"], 0)
        self.assertTrue(entry["fetch_ok"])

    # -- the three thresholds -------------------------------------------------

    def test_old_mid_merge_fails_unit(self):
        repo = self.fresh_repo("mergey")
        head = self.git("rev-parse", "HEAD", cwd=repo).stdout.strip()
        merge_head = os.path.join(repo, ".git", "MERGE_HEAD")
        with open(merge_head, "w") as fh:
            fh.write(head + "\n")
        old = os.path.getmtime(merge_head) - 2 * 3600
        os.utime(merge_head, (old, old))
        proc = self.run_aggregate(repo, "--merge-age-limit", "3600")
        self.assertEqual(proc.returncode, 1, proc.stderr)
        alerts = self.read_report()["alerts"]
        self.assertEqual([a["kind"] for a in alerts], ["mid_merge"])
        self.assertGreaterEqual(alerts[0]["age_seconds"], 3600)
        # The same state inside the limit must not fail the unit.
        proc = self.run_aggregate(repo, "--merge-age-limit", "7200")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(self.read_report()["alerts"], [])

    def test_old_unpushed_commit_fails_unit(self):
        repo = self.fresh_repo("unpushy")
        bare = os.path.join(self.tmp, "unpushy.git")
        self.git("clone", "-q", "--bare", repo, bare, cwd=self.tmp)
        self.git("remote", "add", "origin", bare, cwd=repo)
        self.git("fetch", "-q", "origin", cwd=repo)
        self.git("push", "-q", "-u", "origin", "main", cwd=repo)
        with open(os.path.join(repo, "f.txt"), "a") as fh:
            fh.write("local-only\n")
        self.git("add", "f.txt", cwd=repo)
        when = "2026-01-01T00:00:00+00:00"
        self.git("commit", "-q", "-m", "local-only", cwd=repo,
                 env_dates={"GIT_AUTHOR_DATE": when, "GIT_COMMITTER_DATE": when})
        proc = self.run_aggregate(repo, "--unpushed-age-limit", "3600")
        self.assertEqual(proc.returncode, 1, proc.stderr)
        alerts = self.read_report()["alerts"]
        self.assertEqual([a["kind"] for a in alerts], ["unpushed"])
        self.assertGreaterEqual(alerts[0]["age_seconds"], 3600)
        self.assertEqual(alerts[0]["host"], os.uname().nodename)

    def test_far_behind_fails_unit(self):
        base = self.fresh_repo("behindy-base")
        bare = os.path.join(self.tmp, "behindy.git")
        self.git("clone", "-q", "--bare", base, bare, cwd=self.tmp)
        twin = os.path.join(self.tmp, "behindy-twin")
        self.git("clone", "-q", bare, twin, cwd=self.tmp)
        behindy = os.path.join(self.tmp, "behindy")
        self.git("clone", "-q", bare, behindy, cwd=self.tmp)
        with open(os.path.join(twin, "f.txt"), "a") as fh:
            fh.write("two\n")
        self.git("add", "f.txt", cwd=twin)
        self.git("commit", "-q", "-m", "two", cwd=twin)
        self.git("push", "-q", "origin", "main", cwd=twin)
        proc = self.run_aggregate(behindy, "--behind-limit", "0")
        self.assertEqual(proc.returncode, 1, proc.stderr)
        alerts = self.read_report()["alerts"]
        self.assertEqual([a["kind"] for a in alerts], ["behind"])
        self.assertEqual(alerts[0]["count"], 1)

    # -- informational + walk behaviour ---------------------------------------

    def test_checkpoint_churn_reported_not_alerted(self):
        repo = self.fresh_repo("cp-repo")
        cp = os.path.join(repo, ".beads", "checkpoint")
        os.makedirs(cp)
        with open(os.path.join(cp, "current.json"), "w") as fh:
            fh.write("{}\n")
        self.git("add", "-f", ".beads/checkpoint/current.json", cwd=repo)
        self.git("commit", "-q", "-m", "cp", cwd=repo)
        with open(os.path.join(cp, "current.json"), "a") as fh:
            fh.write("# churn\n")
        proc = self.run_aggregate(repo)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        entry = self.read_report()["hosts"][0]["repos"][0]
        self.assertEqual(entry["checkpoint_dirty"], 1)
        self.assertEqual(self.read_report()["alerts"], [])

    def test_scan_json_is_machine_readable(self):
        repo = self.fresh_repo("jsony")
        proc = self.run_checker("--scan-json", "--scan-root", repo, "--no-fetch")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        doc = json.loads(proc.stdout)
        self.assertEqual(len(doc["repos"]), 1)
        self.assertEqual(doc["repos"][0]["branch"], "main")

    def test_worktree_dedupes_against_parent(self):
        parent = self.fresh_repo("wt-parent")
        child = os.path.join(self.tmp, "wt-child")
        self.git("worktree", "add", "-q", "--detach", child, "HEAD", cwd=parent)
        found, _notes = CD.find_repos(self.tmp, 5, 250, 30)
        real = [os.path.realpath(p) for p in found]
        kept = sum(1 for p in (parent, child) if os.path.realpath(p) in real)
        self.assertEqual(kept, 1, f"parent/child pair kept {kept} entries: {real}")

    def test_walk_prunes_build_and_cache_dirs(self):
        deep = self.fresh_repo("deep")
        nested = os.path.join(deep, "target", "vendored", "x", "y", "z")
        os.makedirs(nested)
        self.git("init", "-q", "-b", "main", cwd=nested)
        found, _notes = CD.find_repos(self.tmp, 6, 250, 30)
        real = [os.path.realpath(p) for p in found]
        self.assertIn(os.path.realpath(deep), real)
        self.assertNotIn(os.path.realpath(nested), real)

    def test_walk_prunes_tmp_and_scratch(self):
        # ~/.tmp is this box's TMPDIR: the first live run found a dozen
        # secret-scanner throwaway clones there. scratch/ is disposable by
        # policy. Neither may surface as drift findings.
        kept = self.fresh_repo("kept")
        junk_dirs = [
            os.path.join(self.tmp, ".tmp", "hook-clone"),
            os.path.join(self.tmp, "tmp", "verify-clone"),
            os.path.join(self.tmp, "scratch", "roundtrip", "ARMOR"),
        ]
        for junk in junk_dirs:
            os.makedirs(junk)
            self.git("init", "-q", "-b", "main", cwd=junk)
        found, _notes = CD.find_repos(self.tmp, 5, 250, 30)
        real = [os.path.realpath(p) for p in found]
        self.assertIn(os.path.realpath(kept), real)
        for junk in junk_dirs:
            self.assertNotIn(os.path.realpath(junk), real)


if __name__ == "__main__":
    unittest.main()
