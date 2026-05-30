# bf-5q7: Splice and Commit-Trailer Documentation - Verification

## Task

Document Splice strand and commit-trailer injection in plan.md.

## Finding

**Both features are already fully documented in docs/plan/plan.md.**

### Splice Strand (Strand 8)

**Location:** lines 1130-1175

The plan includes a complete specification:
- Purpose: worker failure documentation (dead workers and live-but-looping workers)
- Entry conditions: Strand 7 returned no work, Splice enabled, heartbeat files exist
- Algorithm: scan for dead workers (stale heartbeat + dead tmux), scan for live loops (claim churn, state ping-pong, log runaway), create failure beads
- Exit conditions: WorkCreated → restart from Strand 1, NoWork → fall through to Strand 9
- Guardrails: splice_state.json persistence, loop detection thresholds
- Waterfall position: Correctly positioned as Strand 8 (after Reflect, before Knot)

### Commit-Trailer Injection

**Location:** lines 746-783 (commit_hook module specification in Architecture chapter)

The plan includes:
- Trailer format: `Bead-Id: nd-a3f8`
- Trigger: When HEAD moved since `pre_dispatch_head` (agent made commits)
- HOOP integration: HOOP's bead_commit_index picks up via `git log --format=%(trailers:key=Bead-Id,valueonly,separator=,)`
- Timeouts: 10s for git rev-parse, 30s for git commit --amend

### Module Boundary Table

**Location:** lines 438-458

Includes both entries:
```
├── strand/           Strand waterfall evaluation
│   └── splice/       Worker failure documentation
├── commit_hook/      Bead-Id trailer injection for git commits
```

### Strand Numbering Consistency

The waterfall sequence (lines 885-915) and individual strand sections use consistent numbering:
- Strand 1: Pluck (lines 917-940)
- Strand 2: Mend (lines 941-964)
- Strand 3: Explore (lines 965-992)
- Strand 4: Weave (lines 993-1021)
- Strand 5: Unravel (lines 1022-1050)
- Strand 6: Pulse (lines 1051-1078)
- Strand 7: Reflect (lines 1079-1129)
- Strand 8: Splice (lines 1130-1174)
- Strand 9: Knot (lines 1175-end)

## Conclusion

The plan.md documentation is complete and accurate. No changes were required.

All acceptance criteria are already met:
- [x] Plan accurately describes Splice strand algorithm and position in waterfall
- [x] Plan accurately describes commit-trailer injection trigger and format
- [x] No code features are undocumented in the plan
