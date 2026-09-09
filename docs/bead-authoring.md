# Bead Authoring — one deliverable per bead

How to write a bead title that a dispatched agent **executes** instead of
**decomposes**. The agent in the loop, not NEEDLE, decides to spawn sub-beads —
so the fix is in the wording the agent reads.

## The rule

**One deliverable, one acceptance command, per bead.**

- The title names the thing to create and the command that proves it works.
- The body states the work is not to be decomposed.
- Work that genuinely spans several deliverables becomes several beads,
  ordered with `bead dep add <BLOCKED> <BLOCKER>` — not one bead with a list.

## The failure mode

GitHub [#22](https://github.com/jedarden/NEEDLE/issues/22) (needle 0.6.0,
bead-rs 0.2.4) filed this title:

> Add scripts/\_\_init\_\_.py and scripts/money.py providing MINOR_UNITS,
> to_minor, from_minor and convert, so tests/test_money.py passes.

Four deliverables in one title (a package marker, a constants dict, three
functions, and a passing suite). The worker spawned **~16 sub-beads** — some
near-duplicates — and the parent climbed to revision 15 without ever being
worked. Two details make it worse:

- **It decomposes even when the work is already done.** In a second run the
  work was committed, pytest reported `12 passed` and exited 0, and the worker
  still split the parent instead of closing it.
- **A fragmenting run looks healthy.** `needle status` counts claims, so 16
  spawned sub-beads read as rising throughput while the parent starves.

## The verified wording

Same tests, same gate, same workspace — only the wording differed:

```bash
bead create --title "Create scripts/money.py and scripts/__init__.py so that python3 -m pytest -q tests/test_money.py passes. Work on this issue directly." \
  --description "Do not create sub-issues, do not split this work, and do not decompose it into smaller tasks."
```

Behavior with this wording: **zero** sub-beads, the bead closed in **under 60
seconds**, the dependent bead released automatically, and the workspace bead
count stayed flat. The same instruction was then attached to every bead across
five further experiments with no recurrence.

Both halves matter:

- **Single deliverable in the title.** `scripts/money.py` plus its
  `scripts/__init__.py` package marker is one deliverable here — one module,
  proven by one command. Listing the four functions to implement (as the
  trigger title did) is what invites splitting.
- **The explicit no-decompose instruction.** `Work on this issue directly` in
  the title, and the stronger `Do not create sub-issues, do not split this
  work, and do not decompose it into smaller tasks.` in the body. An agent
  that treats decomposition as helpfulness needs to be told otherwise.

## Where this guidance ships

So an agent actually reads it, not just a human browsing `docs/`:

- [`docs/templates/AGENTS-needle.md`](templates/AGENTS-needle.md) — injected
  between `<!-- needle:begin -->` / `<!-- needle:end -->` markers into the
  repo's `AGENTS.md` by `needle init --backend`, where the dispatched agent
  reads it as workspace context.
- [`docs/agent-onboarding.md`](agent-onboarding.md) — the bead-creation step
  of first-run setup.

## Related NEEDLE-side machinery

Wording is the agent-side fix. NEEDLE's own automatic decomposition (mitosis)
is guarded separately — depth limits, parent/child labels, and split rules live
in `src/prompt/mod.rs`; the history of what happens when those guards fail is
in [`docs/notes/mitosis-explosion-postmortem.md`](notes/mitosis-explosion-postmortem.md).
The two failure modes look similar (many sub-beads, starving parent) but have
different levers: mitosis is capped in code, agent decomposition is capped in
the bead's own wording.
