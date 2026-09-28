# Doc Navigation Check

`scripts/check-doc-links.sh` is a deterministic, network-free guard against
stale documentation navigation. It runs in the fast lane of
`scripts/definition-of-done.sh` ("documentation navigation" and its mutation
tests, "documentation navigation checker tests"), so it executes in every
context that invokes the Definition of Done: the pre-commit hook, the
needle-ci verify step, and the NEEDLE close-verification gate — including the
git-less `git archive` extractions the gate uses.

## What it enforces

1. **Every internal Markdown link resolves.** Every `.md` file under `docs/`
   and the repository `README.md` is scanned for inline links, images, and
   reference-style definitions. Each target that is not a pure anchor or a
   schemed URL (`https:`, `mailto:`, ...) must resolve, relative to the
   linking file, to an existing path inside the repository root. A link that
   escapes the repository root is a failure even when the path exists on some
   machine — session transcripts and other untracked local state must not be
   link targets.
2. **The index links each target at most once.** `docs/README.md` is the
   navigation entry point; a target linked from two different rows is stale
   navigation (duplicate rows have crept in more than once historically).
   The fragment is part of the key, so `plan/plan.md` and
   `plan/plan.md#section` are distinct targets.
3. **Curated directories are fully indexed.** Every file in `docs/adr/` and
   `docs/research/` must be reachable from `docs/README.md`. An ADR that
   ships without an index row fails CI.
4. **The documented inventory is accurate.** The index must state, exactly
   once each, and matching the tree:
   - `contains N markdown files` (index prose) and
     `Total markdown files: N` — the count of `.md` files under `docs/`,
     recursively, including the index itself;
   - `ADRs: N` — the count of `.md` files in `docs/adr/`;
   - `Research documents: N` — the count in `docs/research/`;
   - `Operational notes: N` — the count in `docs/notes/`.

   Deleting a statement is also a failure: the summary cannot be silenced by
   removing it.

## Non-goals

- **Anchor validation.** That `page.md#section` names a real heading in
  `page.md` is not checked; only the file target is.
- **External URLs.** Nothing is fetched; the check is offline and
  deterministic.
- **Fenced examples.** Links inside code fences are examples, not
  navigation, and are ignored.
- **Full coverage of `docs/`.** Only `docs/adr/` and `docs/research/` must
  be completely indexed; other documents are indexed selectively and are
  only checked for link validity, not for presence in the index.
- **Percent-encoded paths.** Targets are compared literally after fragment
  and query removal; `%20`-style encoding is not decoded (no current document
  uses it).
- **Ignored checkout artifacts.** In a Git checkout, ignored files are not
  treated as part of the candidate documentation tree; tracked and nonignored
  untracked files are checked. Clean CI archives contain only the committed
  tree and use the same filesystem scan without requiring Git.

## Running and testing

```bash
bash scripts/check-doc-links.sh              # against this repository
NEEDLE_DOCUMENTATION_ROOT=/path/to/root \
  bash scripts/check-doc-links.sh            # against another tree
bash tests/doc-links/run.sh                  # mutation tests for the checker
```

The mutation suite builds fixture trees under a temp directory and asserts
that each contract violation — broken link, escaping link, stale count,
deleted statement, unindexed ADR or research doc, duplicate target, missing
index — is rejected with a specific message.

## Maintenance

When the check fails:

- a **broken link** — fix or remove the link, or restore/rename the target
  file;
- an **unindexed ADR or research document** — add a row to the relevant
  table in `docs/README.md`;
- an **inventory mismatch** — update the counts in `docs/README.md` (both
  the prose count and the File Count Summary) after adding or removing
  documents;
- a **duplicate target** — delete the redundant row; if the second link is
  intentional cross-reference material, keep it as plain text instead (see
  the "2026-08 Notes (Indexed in Investigations)" section for the pattern).
