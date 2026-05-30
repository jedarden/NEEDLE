# E2E Strand Waterfall Test Coverage (bf-1ka)

## Task Summary
Add e2e test for strand waterfall full progression with real br:
1. Empty workspace → all strands return NoWork → EXHAUSTED
2. Verify strand.evaluated telemetry events in correct sequence
3. Verify Knot creates alert bead

## Finding: Tests Already Implemented

The requested functionality already exists in two tests:

### Test 12: `real_br_strand_waterfall_exhaustion` (lines 1330-1441)
- Tests StrandRunner::select() waterfall progression
- Verifies all strands evaluated in correct order (pluck first, knot last)
- Verifies each strand returns "no_work"
- Verifies Knot creates starvation alert bead
- Uses real br (no mocks)

### Test 13: `real_br_strand_waterfall_exhaustion_with_telemetry` (lines 1447-1655)
- Full end-to-end worker run to completion
- Parses telemetry JSONL log file
- Verifies strand.evaluated events exist and are in correct order
- Verifies each strand.evaluated has result: "no_work"
- Verifies worker.exhausted event with full diagnostics
- Verifies Knot creates starvation alert bead
- Uses real br (no mocks)

## Acceptance Criteria Verification

| Criteria | Test 12 | Test 13 | Shell Test (Scenario B) |
|----------|---------|---------|------------------------|
| Verify Pluck→Mend→Explore→Knot→EXHAUSTED sequence | ✓ | ✓ | ✓ |
| strand.evaluated telemetry events in order | ✓ | ✓ | ✓ |
| Worker reaches EXHAUSTED state | ✓ | ✓ | ✓ |
| Knot alert bead created | ✓ | ✓ | ✓ |
| Real br (no mocks) | ✓ | ✓ | ✓ |

## Shell Test Coverage
`tests/e2e/strand_waterfall.sh` Scenario B (lines 249-463):
- Empty workspace with no remote workspaces
- All strands return NoWork → EXHAUSTED
- Verifies strand.evaluated events via jq
- Verifies Knot creates starvation alert bead

## Conclusion
All acceptance criteria are satisfied by existing tests.
No additional implementation required.
