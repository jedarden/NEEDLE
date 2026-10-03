# verify-changes.sh: the change-evidence compatibility hook

Where it lives (needle-e62f940a):

- **Source of truth:** `scripts/verify-changes.sh` in this repo, tracked since
  commit `1eaefdf8` (2026-09-28). There is no generating template. Before that
  commit, the deployed file had no tracked source, which is why the SIGPIPE bug
  could not be found "in the repo".
- **Deployed copy:** `~/.needle/hooks/verify-changes.sh` on codinghome and lab.
  It was installed as an exact copy of the tested script and is byte-identical
  to the repo copy (sha256 `2863823827a4e6b9dd1daa3ddc32201c1ec8d0edca8ee755eeb126e44629308e`,
  checked on both hosts 2026-10-03). After changing the script, reinstall it on
  both hosts and re-check the hash; nothing syncs it automatically.
- **Tests:** `tests/verify-changes/run.sh`, a bash suite in the
  definition-of-done fast lane ("change evidence hook"). It builds isolated real
  Git repos, including a 1,500-commit history and 1,500 untracked
  long-named paths, both larger than a pipe buffer. Those two cases fail against
  the old `pipefail` + `grep -q` shape.

Why the suite is bash: the fast lane also runs in needle-ci's builder image,
which has no `python3`. The original unittest version made every needle-ci
`verify-fast` fail with exit 127 from 2026-09-29 until it was ported
(needle-f20186a8).

Rule for this script and its siblings: under `set -o pipefail`, never feed an
unbounded producer (`git log`, `git status`) into an early-exiting consumer
(`grep -q`, `head`). The consumer exits, the producer gets SIGPIPE, and the
pipeline reports failure even when a match was found. Bound the producer
(`git rev-list --count --max-count=1`) or consume all of its input
(`grep . >/dev/null`).
