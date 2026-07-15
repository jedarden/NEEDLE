# Top-Level Module Test Results - bf-2do3p

**Date:** 2026-07-14

## Summary

Tested the top-level `cli` and `worker` modules as requested. All tests passed successfully.

## Test Results

### CLI Module (`src/cli/mod.rs`)
- **Tests run:** 106
- **Passed:** 106
- **Failed:** 0
- **Ignored:** 0
- **Duration:** ~0.01s

The CLI module tests cover:
- Command-line argument parsing for all subcommands (run, logs, status, stop, doctor, config, upgrade, version, test-agent)
- Config dump and get operations
- Doctor checks (heartbeat, telemetry, lock files, peers, SQLite, workspace, JSONL)
- Worker identifier generation and collision detection
- PID aliveness checks
- Status and logs formatting
- NATO alphabet name generation
- Shell escaping utilities
- Multi-worker coordination
- Worker state serialization

### Worker Module (`src/worker/mod.rs`)
- **Tests run:** 119
- **Passed:** 119
- **Failed:** 0
- **Ignored:** 0
- **Duration:** ~464.01s

The Worker module tests cover:
- State machine transitions (boot → selecting → claiming → dispatching → executing → handling)
- Budget checking and stop/warn behavior
- Auto-canary promotion logic
- Claim race handling and exclusion tracking
- Retry logic with backoff and max retries
- Routing rule application (baseline, strict mode, first-match wins)
- Adapter resolution and provider configuration
- Workspace handling (cross-workspace beads, home workspace)
- Full cycle integration tests with echo agent
- Worker registry and health checks
- Idle worker flagging and cleanup
- Bead store persistence

## Conclusion

Both top-level modules have comprehensive test coverage and all tests pass without failures. The worker module includes several integration tests that exercise the full bead processing lifecycle, which accounts for the longer execution time (~7.7 minutes). These are the highest-level modules in the codebase, aggregating functionality from all other modules.
