# bf-2weya: Make recursive workspace discovery the intended default

## Summary

This bead completes the documentation and warning infrastructure for making
recursive workspace discovery the intended default behavior for NEEDLE's
Explore strand, while keeping `explore.workspaces` as a pin/exception mechanism.

## Changes Made

### 1. Enhanced ExploreConfig Documentation (src/config/mod.rs)

- Added comprehensive module-level documentation explaining:
  - **Default mode (recommended):** Empty `workspaces` → recursive discovery
  - **Pinned mode (exception):** Non-empty `workspaces` → restricted scan
  - Historical rationale from 2026-07-19 fleet incident

- Updated field documentation for `workspaces` to explicitly state:
  - It's a **pin/exception list**, not a required configuration
  - Empty = recursive discovery (fleet default)
  - Non-empty = restricted mode (WARN log emitted)

### 2. Updated ExploreStrand Module Docs (src/strand/explore.rs)

- Expanded module documentation with:
  - Clear explanation of default vs. pinned modes
  - Explicit statement that recursive discovery is the "intended default for the fleet"
  - Warning that pinned mode is an "exception mechanism"

### 3. Added Startup WARN Log (src/strand/explore.rs:91-108)

- When `config.workspaces` is non-empty, emit a structured WARN log at startup:
  - Names the pinned repos (extracted from paths)
  - Shows worker ID and mode
  - Explains this is an exception to the fleet default
  - Encourages operator to verify intent

## Acceptance Criteria Met

✓ Documentation in ExploreConfig and operator-facing docs states explicitly that
  `workspaces` is a pin/exception field, and empty (default) enables recursive
  discovery as the intended default.

✓ No functional code change required for empty-list case (already worked).

✓ Startup WARN log added when `workspaces` is non-empty, naming the pinned repos
  so operators can immediately see restricted/exception mode.

## Testing

All 26 existing explore strand unit tests pass:
- `empty_workspaces_config_triggers_discovery` ✓
- `explicit_workspaces_list_skips_discovery` ✓
- `discover_workspaces_finds_dirs_with_beads_subdir` ✓
- And 23 other tests ✓

## Rationale

The 2026-07-19 fleet incident occurred because `explore.workspaces` was populated
with 24 hardcoded paths. This permanently disabled discovery fleet-wide, and the
list had already drifted stale (missing valid repos like commitgraph and
twitterapi-proxy). By documenting recursive discovery as the intended default and
warning when workers run in pinned mode, future operators will be less likely to
repeat this configuration error.
