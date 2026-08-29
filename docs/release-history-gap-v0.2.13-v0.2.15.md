# Release History Gap: v0.2.13-v0.2.15

## Summary

Versions v0.2.13, v0.2.14, and v0.2.15 were developed and tagged with commits, but were **never published as GitHub releases** due to a CI failure. This document explains the gap and why these versions were not retroactively published.

## Timeline

- **v0.2.12** - Published successfully (2026-06-14)
- **v0.2.13** - Intended but not published (~2026-07-28)
- **v0.2.14** - Intended but not published (2026-07-30)
- **v0.2.15** - Intended but not published (2026-07-31)
- **v0.2.16** - Published successfully (2026-08-02)

## Root Cause

The needle-ci workflow had a bug where it created draft releases instead of published releases. This is documented in commit e417d4ec: "needle-ci ships draft releases, upgrade path dead since v0.2.13".

The bug was fixed for v0.2.16 and subsequent releases, but v0.2.13-v0.2.15 were lost in the gap.

## What Was Lost

The CHANGELOG entries for these versions show they included important fixes:

### v0.2.13
- OTLP Sink work (features not fully documented due to CHANGELOG lag)
- Version bump commit: eb2e4bce

### v0.2.14 (2026-07-30)
- Shipped-work enforcement (worker.enforce_shipped_work config)
- Adapter routing wired into dispatch
- Failure-quarantine circuit breaker
- Explore strand roam-rotation starvation fix
- Mitosis timeout child-process leak fix
- Orphaned dispatch children fix
- Pre-existing clippy lint cleanup
- Version bump commit: d67c5d9e

### v0.2.15 (2026-07-31)
- Supervisor zombie-child reaping (GH #12, ADR-010)
- Zombie-aware is_pid_alive
- Version bump commit: 563c7172

## Impact Assessment

### For Users

**Minimal impact.** Users can upgrade directly from v0.2.12 → v0.2.16 (or newer) without missing any functionality. All fixes from the missing versions are cumulative in v0.2.16.

### For Operators

**No action needed.** The git history and CHANGELOG preserve the record of what was developed, even though the releases were never published.

## Why Not Retroactively Publish?

The decision was made **NOT** to retroactively publish v0.2.13-v0.2.15 for several reasons:

1. **No original artifacts exist** - The binaries and checksums from those exact build moments are lost. Any recreation would be artificial.

2. **v0.2.16 is superseding** - All fixes from the missing versions are already included in v0.2.16, so there's no functional gap.

3. **Accuracy concerns** - Retroactively creating releases would not accurately represent what was shipped at that time.

4. **CI issue is resolved** - The needle-ci bug that caused this is fixed, so future releases will publish correctly.

## Recommendations

- **Users**: Upgrade to the latest published release (v0.2.16 or newer)
- **Operators**: Reference the CHANGELOG and git history when investigating what features were added when
- **Developers**: Ensure CI tests verify that releases are actually published, not just created as drafts

## References

- Commit e417d4ec: "needle-ci ships draft releases, upgrade path dead since v0.2.13"
- CHANGELOG.md: Detailed entries for v0.2.13-v0.2.15
- Git history: Version bump commits eb2e4bce, d67c5d9e, 563c7172
- Issue: bead needle-e7770cbd (decision on retroactive publishing)

---

**Documented**: 2026-08-29
**Status**: Gap acknowledged, no action taken
