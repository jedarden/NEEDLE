# Pluck Pre-Close Verification Checklist

The Pluck prompt now puts bead closure behind an ordered verification
checklist. Agents must confirm that the working tree contains only their own
changes, run the repository's definition of done from a fresh `git archive HEAD`
extraction, run every command named by the bead's acceptance criteria in
that same extraction, and verify that their commits are pushed with an empty
`git rev-list origin/<branch>..HEAD`. Only then may the agent close with a
`--reason` ending in the fenced `verified:` evidence block.

This closes the workflow gap exposed by the 2026-08-28 ARMOR incidents. The
false-close audit found ARMOR beads marked complete even though their committed
state failed clean verification, including `armor-f35a629f`, whose closing
commit still had compile errors and omitted a required dependency file. A
working-tree test could hide both an uncommitted dependency and uncommitted
fixes. Making the clean extraction and acceptance checks an explicit
pre-close obligation gives the agent a chance to catch those failures before
NEEDLE reopens the bead and counts it toward quarantine.

The contract is kept in sync across the built-in template in
`src/prompt/mod.rs`, the architecture copy in `docs/plan/plan.md`, and the
marathon instructions in `docs/marathon-instruction.md`.
