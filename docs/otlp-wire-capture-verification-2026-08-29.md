# OTLP Wire Capture Verification

**Date:** 2026-08-29
**Task:** Verify workspace attribute changes via OTLP wire capture (bead: needle-abb30b79)

## Test Setup

1. **OTLP Capture Server:** Python HTTP server listening on `localhost:14317`
2. **NEEDLE Configuration:**
   - OTLP enabled: `http://localhost:14317`
   - Protocol: HTTP
   - Signals: logs, metrics, traces all enabled
3. **Test Duration:** 45 seconds
4. **Environment:** Isolated test environment (`HOME=/tmp/needle-test-env`)

## Captured Traffic

### Summary
- **Total requests captured:** 3
- **Endpoint:** `/v1/logs` (OpenTelemetry Logs API)
- **Encoding:** `application/x-protobuf`
- **Timestamps:** 2026-08-29T17:41:43 to 2026-08-29T17:42:13

### Resource Attributes (Static per Process)

All captured requests show the following Resource attributes:
- `service.name=needle`
- `service.version=0.5.0`
- `service.namespace=needle-fleet`
- `service.instance.id=claude-alpha`
- `needle.agent=claude`
- `needle.session_id=f23a2562`
- **`needle.workspace=NEEDLE`** ✅
- `telemetry.sdk.name=opentelemetry`
- `telemetry.sdk.language=rust`
- `telemetry.sdk.version=0.31.0`

### Verification Results

#### ✅ Attribute Exists
The `workspace` attribute is present in all OTLP log records as:
- **Resource attribute:** `needle.workspace=NEEDLE`
- **Value format:** Basename only (no full filesystem paths)

#### ✅ Zero Full Paths
No full filesystem paths detected in any captured records. The workspace attribute contains only `NEEDLE` (the basename), confirming the implementation of ADR-016 Decision #3.

#### ❓ Per-Record Workspace Attribute
The current wire capture shows **Resource-level** `workspace` attributes, but does not yet show **per-record** `workspace` attribute changes because:

1. **Agent adapter error:** Needle startup failed with `Configured agent adapter 'claude' was not found`
2. **No beads processed:** The worker never claimed/processed beads from different repos
3. **No roaming behavior:** Without successful bead processing, the worker never roamed between repos

## Code Analysis

Based on source code analysis of `src/telemetry/otlp.rs` and `src/claim/mod.rs`:

### Per-Record Workspace Attribute Implementation

**Location:** `src/telemetry/otlp.rs:1483-1488`
```rust
if let Some(ref workspace) = event.workspace {
    attrs.push((
        "workspace",
        workspace_label(&workspace.to_string_lossy()).into(),
    ));
}
```

**Workspace Update Mechanism:** `src/claim/mod.rs:318, 659`
```rust
self.telemetry.set_workspace(bead.workspace.clone());
```

**Key Findings:**
1. Per-record `workspace` attribute is emitted when `event.workspace` is present
2. Workspace is set from `bead.workspace` when beads are claimed
3. Each bead carries its own workspace path
4. `workspace_label()` function extracts basename only (lines 73-80)

### Expected Behavior

When a worker processes beads from multiple repos:
1. Bead A from `/home/coding/repo1` → per-record `workspace=repo1`
2. Bead B from `/home/coding/repo2` → per-record `workspace=repo2`
3. Resource attribute `needle.workspace` remains static for the process

## Acceptance Criteria Status

| Criterion | Status | Evidence |
|-----------|--------|----------|
| Raw OTLP payload shows `workspace` attribute | ✅ YES | Wire capture shows `needle.workspace=NEEDLE` |
| At least two different workspace basenames appear | ❓ PENDING | Need successful bead processing |
| Zero full filesystem paths | ✅ YES | All attributes use basename only |
| Evidence saved to docs/ | ✅ YES | This document |

## Next Steps

To complete the verification, we need to:

1. **Fix agent adapter configuration** - The test config used `provider: claude-code-glm-4.7` but the runtime looked for `claude`
2. **Process actual beads** - Let needle claim and work beads from multiple repos
3. **Capture per-record attributes** - Verify the per-record `workspace` attribute changes between repos

## Technical Notes

### OTLP Protobuf Encoding

The captured traffic uses protobuf encoding (`application/x-protobuf`), which requires proper decoding to extract individual log records and their attributes. The body preview shows binary data mixed with UTF-8 strings.

### Workspace Label Function

From `src/telemetry/otlp.rs:73-80`:
```rust
fn workspace_label(workspace: &str) -> String {
    Path::new(workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(workspace)
        .to_string()
}
```

This function ensures that only the final path component (basename) is used in OTLP attributes, preventing filesystem path leakage.

## Conclusion

**Partial Success:** The wire capture confirms that:
- ✅ Workspace attributes are emitted in OTLP traffic
- ✅ Only basenames are used (no full paths)
- ❓ Per-record workspace attribute changes require successful bead processing

The implementation appears correct based on code analysis, but full verification requires successful worker operation with bead processing across multiple repos.
