# Pulse Strand Enablement - NEEDLE Workspace

**Date:** 2026-08-15  
**Bead:** needle-82cf55c1

## Problem

NEEDLE's own workspace exhibited ready-bead starvation: 0 ready beads despite 70+ beads in open status, with ~30 beads marked as blocked. This matches the fleet-wide pattern documented in `memory/needle_lab_saturation_limits` — the fleet is capped by ready-bead supply, not hardware.

## Solution

Enabled the Pulse strand (codebase health scanner) in `.needle.yaml` with conservative starting configuration.

## Configuration

```yaml
strands:
  pulse:
    enabled: true
    scanners:
      - name: todo-comments
        command: grep -rn "TODO\|FIXME\|XXX\|HACK" src/ --include="*.rs" || true
        severity_threshold: 4
      - name: unwrap-usage
        command: grep -rn "\.unwrap()" src/ --include="*.rs" | grep -v "test\|test_modules" || true
        severity_threshold: 3
      - name: large-files
        command: find src/ -name "*.rs" -size +50k -exec ls -lh {} \; || true
        severity_threshold: 4
    max_beads_per_run: 3
    cooldown_hours: 24
    severity_threshold: 3
```

### Scanner Rationale

1. **todo-comments** (severity 4): Tracks technical debt markers for future cleanup
2. **unwrap-usage** (severity 3): Identifies potential panic points in non-test code (excludes test modules)
3. **large-files** (severity 4): Flags files >50KB that may benefit from splitting

### Conservative Settings

- **max_beads_per_run: 3** — Low volume to avoid flooding the workspace
- **cooldown_hours: 24** — Daily scans prevent redundant bead creation
- **severity_threshold: 3** — Only creates beads for severity 1-3 (critical to moderate)

## Scanner Output Validation

**Tested 2026-08-15:**

### todo-comments
- Found 8 instances (TODO, FIXME markers)
- Actionable items: heartbeat cleanup, Windows liveness check, test re-enablement
- **Verdict:** Non-noisy, genuinely actionable

### unwrap-usage  
- Found unwrap() calls primarily in test modules (correctly excluded by grep filter)
- Non-test usage appears in serialization contexts where failure is truly exceptional
- **Verdict:** Appropriate signal, test exclusion working as intended

### large-files
- Found 5 files >50KB
- Largest: `src/config/mod.rs` (252K) — candidate for module decomposition
- **Verdict:** Objective metric, useful refactoring signal

## Expected Impact

- **Ready bead supply:** Pulse will create 1-3 beads per 24-hour cycle from codebase health findings
- **Noise resistance:** Deduplication prevents repeated beads for the same issue
- **Severity filtering:** Only moderate-to-critical issues (1-3) generate beads

## Next Steps

1. Monitor first Pulse run output for bead quality
2. If noise-free, consider expanding scanners to include:
   - `cargo clippy` (once compilation errors are fixed)
   - Test coverage gaps
   - Unused dependencies
3. If beads are too generic, tighten severity thresholds

## References

- Pulse implementation: `src/strand/pulse.rs` (~1050 lines, fully implemented)
- Memory: `needle_lab_saturation_limits` — "fleet capped by READY-BEAD supply not hardware"
- Task: needle-82cf55c1
