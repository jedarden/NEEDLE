# NEEDLE Gap Analysis Report

**Generated:** 2026-03-07
**Parent Bead:** nd-12nu

## Executive Summary

This gap analysis compares the current NEEDLE implementation against `docs/plan.md`. The analysis identified 14 gaps across multiple categories:

| Category | Gaps Found | Priority Range |
|----------|------------|----------------|
| CLI Commands | 2 | P2-P3 |
| Strand System | 4 | P1-P2 |
| Hook System | 1 | P2 |
| Watchdog/Recovery | 1 | P1 |
| Telemetry | 1 | P1 |
| Self-Update | 1 | P2 |
| Billing Models | 1 | P2 |
| Infrastructure | 3 | P1-P3 |

## Implementation Status

### ✅ Fully Implemented

| Component | Location | Notes |
|-----------|----------|-------|
| CLI: init | src/cli/init.sh | Interactive onboarding |
| CLI: run | src/cli/run.sh | Multi-worker spawning |
| CLI: list | src/cli/list.sh | Worker listing |
| CLI: status | src/cli/status.sh | Health dashboard |
| CLI: config | src/cli/config.sh | Config management |
| CLI: logs | src/cli/logs.sh | Log viewing |
| CLI: version | src/cli/version.sh | Version display |
| CLI: upgrade | src/cli/upgrade.sh | Self-update |
| CLI: rollback | src/cli/rollback.sh | Version rollback |
| CLI: agents | src/cli/agents.sh | Agent listing |
| CLI: attach | src/cli/attach.sh | tmux attach |
| CLI: stop | src/cli/stop.sh | Worker stop |
| CLI: restart | src/cli/restart.sh | Worker restart |
| CLI: test-agent | src/cli/test-agent.sh | Agent testing |
| CLI: setup | src/cli/setup.sh | Dependency installer |
| CLI: help | src/cli/help.sh | Help system |
| Strand 1: Pluck | src/strands/pluck.sh | Bead claiming |
| Strand 2: Explore | src/strands/explore.sh | Workspace discovery |
| Strand 3: Mend | src/strands/mend.sh | Maintenance |
| Mitosis | src/bead/mitosis.sh | Bead decomposition |
| Heartbeat | src/watchdog/heartbeat.sh | Worker health |
| Agent Adapters | config/agents/*.yaml | Claude, OpenCode, Codex, Aider |

### ⚠️ Partially Implemented / Needs Verification

| Component | Gap | Bead |
|-----------|-----|------|
| Strand 4: Weave | Doc gap analysis needs verification | nd-3a47 |
| Strand 5: Unravel | HUMAN alternatives need verification | nd-368f |
| Strand 6: Pulse | Not connected to worker loop | nd-hq68 |
| Hook System | All 8 hook points need verification | nd-1z0x |
| Concurrency Limits | Enforcement needs verification | nd-30t6 |
| Mitosis Config | Settings may be incomplete | nd-qz4k |

### ❌ Missing from Implementation

| Component | Plan Reference | Bead | Priority |
|-----------|---------------|------|----------|
| `needle pulse` CLI | Lines 1971-2090 | nd-3jns | P2 |
| Shell completion | CLI Help System | nd-306r | P3 |
| Auto-recovery respawn | Lines 2471-2646 | nd-2t2x | P1 |
| Token extraction | Logging & Telemetry | nd-2rzf | P1 |
| Version check on startup | Lines 414-640 | nd-16sm | P2 |
| One-liner installer | Lines 165-194 | nd-edb2 | P1 |
| Billing model profiles | Lines 2223-2253 | nd-91x4 | P2 |
| Release workflow | Implied | nd-22ne | P1 |
| zai-glm5 agent | Lines 2769-2790 | nd-3986 | P3 |
| CLI help text completeness | Lines 672-1420 | nd-3rfc | P3 |
| Pulse detectors | Lines 1992-2016 | nd-1oaq | P2 |

## Bead Dependency Graph

```
nd-edb2 (Installer)
    └── nd-22ne (Release workflow)

nd-2rzf (Telemetry)
    └── nd-hq68 (Pulse strand)
        ├── nd-3jns (CLI pulse)
        └── nd-1oaq (Pulse detectors)

nd-3a47 (Weave) ─┐
nd-368f (Unravel)├── nd-91x4 (Billing profiles)
nd-hq68 (Pulse)  ┘

nd-3rfc (CLI help)
    └── nd-306r (Shell completion)
```

## Created Beads Summary

| Bead ID | Title | Priority |
|---------|-------|----------|
| nd-3jns | CLI: Add needle pulse command | P2 |
| nd-306r | CLI: Implement shell completion for bash/zsh | P3 |
| nd-3a47 | Strand 4 (Weave): Complete doc gap analysis | P2 |
| nd-368f | Strand 5 (Unravel): Complete HUMAN alternatives | P2 |
| nd-hq68 | Strand 6 (Pulse): Connect to worker loop | P1 |
| nd-1z0x | Hook System: Verify all hook points | P2 |
| nd-2t2x | Watchdog: Implement auto-recovery respawn | P1 |
| nd-2rzf | Telemetry: Implement token extraction | P1 |
| nd-16sm | Self-Update: Implement version check on startup | P2 |
| nd-edb2 | Installer: Create one-liner install script | P1 |
| nd-91x4 | Billing: Implement billing model profiles | P2 |
| nd-22ne | GitHub Actions: Create release workflow | P1 |
| nd-3986 | Agent: Add zai-glm5 agent configuration | P3 |
| nd-qz4k | Config: Add mitosis configuration settings | P2 |
| nd-3rfc | Docs: Add all CLI help text from plan | P3 |
| nd-1oaq | Pulse: Implement all detectors | P2 |
| nd-30t6 | Concurrency: Verify limit enforcement | P1 |

## Priority Breakdown

| Priority | Count | Focus |
|----------|-------|-------|
| P1 (Critical) | 6 | Core functionality |
| P2 (High) | 8 | Important features |
| P3 (Normal) | 3 | Enhancements |

## Recommendations

### Immediate (P1)
1. **nd-2rzf** - Token extraction is fundamental to effort logging
2. **nd-2t2x** - Auto-recovery is critical for production reliability
3. **nd-hq68** - Pulse strand enables proactive codebase monitoring
4. **nd-edb2** - One-liner install is essential for adoption
5. **nd-22ne** - Release workflow enables distribution
6. **nd-30t6** - Concurrency enforcement prevents resource exhaustion

### Short-term (P2)
1. Complete strand implementations (Weave, Unravel, Pulse)
2. Implement billing model profiles
3. Add version check on startup
4. Verify hook system completeness

### Long-term (P3)
1. Complete shell completion
2. Add remaining agent configurations
3. Ensure CLI help text matches plan

## Files Reviewed

- `docs/plan.md` - Full implementation specification
- `bin/needle` - Main CLI entry point
- `src/cli/*.sh` - All CLI subcommands
- `src/strands/*.sh` - Strand implementations
- `src/bead/*.sh` - Bead management
- `src/runner/*.sh` - Worker loop and state
- `src/watchdog/*.sh` - Heartbeat and monitoring
- `src/telemetry/*.sh` - Effort logging
- `src/hooks/*.sh` - Hook system
- `config/agents/*.yaml` - Agent configurations
- `tests/*.sh` - Test suites
