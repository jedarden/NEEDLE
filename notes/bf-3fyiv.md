# Unit Test Results - bf-3fyiv

## Summary
All 1366 unit tests passed successfully.

**Test Run Date:** 2026-07-14  
**Total Duration:** 616.73 seconds (~10.3 minutes)  
**Result:** ✅ PASSED (1366 passed; 0 failed; 0 ignored; 0 filtered out)

## Module Breakdown

| Module | Tests | Description |
|--------|-------|-------------|
| config | 103 | Configuration parsing and validation |
| cli | 101 | Command-line interface handling |
| routing | 77 | Request routing logic |
| telemetry | 66 | Metrics and event emission |
| cargo_test | 65 | Test execution framework |
| dispatch | 64 | Agent dispatch coordination |
| types | 55 | Core type definitions and serialization |
| health | 45 | Health check system |
| canary | 33 | Canary deployment system |
| prompt | 29 | Prompt generation |
| validation | 26 | Input validation gates |
| stats | 26 | Statistics tracking |
| bead_store | 25 | Bead storage and parsing |
| outcome | 24 | Outcome handling |
| cost | 23 | Cost calculation |
| worker | 20 | Worker state machine |
| mitosis | 20 | Binary update/fork logic |
| trace | 19 | Trace file handling |
| upgrade | 17 | Upgrade checks |
| skill | 17 | Skill system |
| transcript | 16 | Transcript handling |
| rate_limit | 16 | Rate limiting |
| sanitize | 15 | Input sanitization |
| registry | 15 | Service registry |
| claude_md_placement | 14 | CLAUDE.md file placement |
| claim | 12 | Bead claiming logic |
| test_output | 11 | Test output parsing |
| strand | 11 | Strand execution |
| peer | 11 | Peer communication |
| learning | 10 | Learning/memory system |
| drift | 9 | Drift detection |
| decision | 8 | Decision logic |
| agent_event | 6 | Agent event types |
| span | 4 | Span tracking |
| supervisor | 2 | Supervisor logic |
| commit_hook | 2 | Commit hooks |

## Acceptance Criteria Verification

✅ All unit tests pass (1366/1366)  
✅ Test results captured  
✅ No failing unit tests

## Execution Details

- Command: `cargo test --lib`
- Environment: Local development environment
- Rust toolchain: 1.75+ (as specified in rust-toolchain.toml)
