"""Exercise the deployed compatibility gate against isolated real Git repos."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(os.environ.get("VERIFY_CHANGES_SCRIPT", "scripts/verify-changes.sh")).resolve()


class ChangeGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="needle-change-gate-")
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name)
        self.root = base / "repo"
        self.root.mkdir()
        # The pre-commit gate runs with a captured index/worktree in GIT_*.
        # Fixture commands must never inherit those or the operator's config.
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        self.env.update(HOME=str(base / "home"), XDG_CONFIG_HOME=str(base / "config"))
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        (self.root / "source.txt").write_text("before\n")
        self.git("add", "--", "source.txt")
        self.git("commit", "-q", "-m", "initial")
        self.marker = self.root / ".needle-predispatch-sha"
        self.marker.write_text(self.git("rev-parse", "HEAD"))

    def git(self, *args, input=None):
        return subprocess.run(
            ["git", *args], cwd=self.root, input=input, text=True,
            capture_output=True, check=True, env=self.env,
        ).stdout.strip()

    def gate(self):
        return subprocess.run(
            ["bash", str(SCRIPT)], cwd=self.root, timeout=15, env=self.env,
        ).returncode

    def test_marker_alone_is_not_work(self):
        self.assertEqual(self.gate(), 1)

    def test_missing_marker_preserves_legacy_compatibility(self):
        self.marker.unlink()
        self.assertEqual(self.gate(), 0)

    def test_committed_work_with_history_larger_than_pipe_buffer(self):
        parent = self.git("rev-parse", "HEAD")
        message = "committed work " + "x" * 300 + "\n"
        stream = []
        for i in range(1500):
            stream.append(
                f"commit refs/heads/main\nmark :{i + 1}\n"
                "committer Fixture <fixture@example.invalid> 1700000000 +0000\n"
                f"data {len(message)}\n{message}"
                f"from {parent if i == 0 else ':' + str(i)}\n\n"
            )
        self.git("fast-import", "--quiet", input="".join(stream))
        for _ in range(3):
            self.assertEqual(self.gate(), 0)

    def test_tracked_change_passes_before_and_after_staging(self):
        (self.root / "source.txt").write_text("after\n")
        self.assertEqual(self.gate(), 0)
        self.git("add", "--", "source.txt")
        self.assertEqual(self.gate(), 0)

    def test_untracked_listing_larger_than_pipe_buffer(self):
        for i in range(1500):
            (self.root / (f"{i:04d}-" + "x" * 160)).touch()
        self.assertEqual(self.gate(), 0)


if __name__ == "__main__":
    unittest.main()
