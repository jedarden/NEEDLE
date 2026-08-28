# Capabilities Negotiation Contract

## Purpose

The `bead capabilities` command is the **runtime capability negotiation interface** between NEEDLE and bead-rs backends. It allows NEEDLE to:

1. **Verify backend identity** — confirm the binary is actually bead-rs, not another CLI
2. **Assert safety guarantees** — ensure required features (atomic_claim) are present
3. **Validate schema compatibility** — confirm the backend uses supported data schemas
4. **Detect version drift** — fail closed when capabilities change across versions

This is **NOT** optional — NEEDLE refuses to open a bead-rs workspace if capabilities negotiation fails or returns unexpected values.

## When Capabilities Are Probed

Capabilities are checked **once during worker initialization**, in `bead_store::open_configured()`:

```rust
// src/bead_store/mod.rs:257-327
fn verify_bead_rs_capabilities(binary: &Path, workspace: &Path) -> Result<()> {
    let output = Command::new(binary)
        .args(["capabilities", "--profile", "native-v1"])
        .current_dir(workspace)
        .output()?;

    // Parse and validate JSON response
    let capabilities: serde_json::Value = serde_json::from_slice(&output.stdout)?;

    // Required checks (fail closed if any fail):
    // 1. implementation == "bead-rs"
    // 2. atomic_claim == true
    // 3. statuses contains all required values
    // 4. schemas contains all required URNs
}
```

## Required Command Signature

```bash
bead capabilities --profile native-v1
```

- **Must execute successfully** — non-zero exit code causes workspace open to fail
- **Must return valid JSON** on stdout — parse errors cause workspace open to fail
- **Must be run in workspace directory** — capabilities may vary per workspace

## Required JSON Structure

The command MUST return a JSON object with the following top-level fields:

```json
{
  "implementation": "bead-rs",
  "atomic_claim": true,
  "statuses": ["open", "in_progress", "deferred", "closed"],
  "schemas": [
    {"schema_ref": "urn:bead-rs:schema:issue:native-v1"},
    {"schema_ref": "urn:bead-rs:schema:event:native-v1"},
    {"schema_ref": "urn:bead-rs:schema:field-guide:native-v1"}
  ]
}
```

### Field-by-Field Requirements

#### `implementation` (string)
- **Required value:** `"bead-rs"`
- **Purpose:** Backend identity verification
- **Validation:** Exact string match
- **Failure mode:** Workspace open fails with "capability mismatch" error

```rust
// src/bead_store/mod.rs:284-288
if capabilities.get("implementation").and_then(|v| v.as_str()) != Some("bead-rs") {
    bail!("bead-rs capability mismatch for workspace {}: expected implementation=bead-rs");
}
```

#### `atomic_claim` (boolean)
- **Required value:** `true`
- **Purpose:** Asserts that the backend provides **atomic claim operations**
- **Why this is critical:** Without atomic claims, multiple workers can race to claim the same bead, leading to duplicate work and lost updates
- **Validation:** Must be exactly `true` (truthy values are insufficient)
- **Failure mode:** Workspace open fails with "capability mismatch" error

```rust
// src/bead_store/mod.rs:288-292
if capabilities.get("atomic_claim").and_then(|v| v.as_bool()) != Some(true) {
    bail!("bead-rs capability mismatch for workspace {}: expected implementation=bead-rs and atomic_claim=true");
}
```

**Atomic claim guarantee:** When `atomic_claim` is true, the backend's `claim` or `claim_auto` operation MUST:
- Execute within a single database transaction (BEGIN IMMEDIATE or equivalent)
- Select and assign the bead atomically — no race window between read and write
- Return a deterministic outcome: success (bead claimed), or failure (already claimed/doesn't exist)

#### `statuses` (array of strings)
- **Required values:** All four statuses MUST be present
  - `"open"` — Bead is ready to be claimed
  - `"in_progress"` — Bead is currently assigned and being worked
  - `"deferred"` — Bead is intentionally deferred (not blocked, just postponed)
  - `"closed"` — Bead is complete
- **Purpose:** Validate that the backend's status model matches NEEDLE's expectations
- **Validation:** Each required status must be present in the array
- **Failure mode:** Workspace open fails with "missing status {status}" error

```rust
// src/bead_store/mod.rs:298-308
for status in ["open", "in_progress", "deferred", "closed"] {
    let present = capabilities["statuses"]
        .as_array()
        .is_some_and(|values| values.iter().any(|value| value == status));
    if !present {
        bail!("bead-rs capability mismatch for workspace {}: missing status {status}");
    }
}
```

#### `schemas` (array of objects)
- **Required schema URNs:** All three MUST be present
  - `"urn:bead-rs:schema:issue:native-v1"` — Issue/bead data schema
  - `"urn:bead-rs:schema:event:native-v1"` — Event history schema
  - `"urn:bead-rs:schema:field-guide:native-v1"` — Field definitions/schema
- **Purpose:** Validate that the backend's data serialization format is compatible
- **Validation:** Each schema_ref must be present in the schemas array
- **Failure mode:** Workspace open fails with "missing schema {schema_ref}" error

```rust
// src/bead_store/mod.rs:309-325
for schema_ref in [
    "urn:bead-rs:schema:issue:native-v1",
    "urn:bead-rs:schema:event:native-v1",
    "urn:bead-rs:schema:field-guide:native-v1",
] {
    let present = capabilities["schemas"].as_array().is_some_and(|schemas| {
        schemas.iter().any(|schema| schema["schema_ref"] == schema_ref)
    });
    if !present {
        bail!("bead-rs capability mismatch for workspace {}: missing schema {schema_ref}");
    }
}
```

## Backend Descriptor Capabilities

The static backend descriptor (`BeadBackendCapabilities`) declares **compile-time** capabilities that are cross-checked against the runtime capabilities probe:

```rust
// src/bead_store/backend.rs:86-94
pub struct BeadBackendCapabilities {
    pub atomic_claim: bool,           // Backend supports atomic claim ops
    pub transactional_batch: bool,    // Backend supports transactional batch (split/mitosis)
    pub velocity_metadata: bool,      // Backend includes model/harness metadata in claims
}
```

### bead-rs (verified against v0.1.3)

```rust
capabilities: BeadBackendCapabilities {
    atomic_claim: true,           // ✅ Verified via capabilities probe
    transactional_batch: false,    // ❌ Sequential split, NOT crash-safe
    velocity_metadata: false,      // ❌ Claims omit model/harness info
}
```

**Implications:**
- ✅ Atomic claims prevent duplicate work in multi-worker scenarios
- ❌ Split operations are **NOT crash-safe** — a crash mid-split leaves orphaned children
- ❌ Claims don't record which model/harness claimed the bead (limitation for velocity tracking)

### bead-forge (verified against v0.4.1)

```rust
capabilities: BeadBackendCapabilities {
    atomic_claim: true,           // ✅ Verified via descriptor only (no probe)
    transactional_batch: true,     // ✅ Atomic split/mitosis
    velocity_metadata: true,       // ✅ Claims include model/harness metadata
}
```

**Implications:**
- ✅ Atomic claims prevent duplicate work
- ✅ Split operations are **crash-safe** — all-or-nothing transaction
- ✅ Full velocity tracking (which agent claimed which bead)

## Capability Gaps and Their Impact

### Missing `transactional_batch`

When a backend lacks transactional batch support:

```rust
// src/bead_store/mod.rs:1385-1404
async fn split_bead(&self, parent_id: &BeadId, children: &[NewChild<'_>]) -> Result<Vec<BeadId>> {
    // Default implementation: sequential, non-atomic
    let mut created = Vec::with_capacity(children.len());
    for child in children {
        let child_id = self.create_bead(child.title, child.body, child.labels).await?;
        self.add_dependency(&child_id, parent_id).await?;
        created.push(child_id);
    }
    Ok(created)
}
```

**Failure mode:** If the process crashes between `create_bead` and `add_dependency`:
1. Child bead exists but has no dependency link to parent
2. Parent never unblocks (still waiting on child that will never complete)
3. Plan phase deadlocks — Phase 5.3, Race 3 from plan.md

**Detection:** NEEDLE's `check` subcommand warns about this gap:

```rust
// src/cli/mod.rs:4067-4068
if !descriptor.capabilities.transactional_batch {
    detail.push("capability gap: split/mitosis is sequential, not atomic".to_string());
}
```

### Missing `velocity_metadata`

When a backend lacks velocity metadata support:

**Impact:** Claims don't record:
- Which model was used (e.g., "claude-opus-5" vs "claude-haiku-4-5-20251001")
- Which harness/version invoked the claim (e.g., "claude-code" vs manual agent)

**Limitation:** Cannot correlate failures with specific model versions or harness configurations.

## Error Handling

All capability checks use **fail-closed** semantics:

```rust
if !valid {
    bail!("bead-rs capability mismatch for workspace {}: ...", workspace.display());
}
```

This means:
- Missing capabilities → workspace won't open
- Malformed JSON → workspace won't open
- Wrong backend type → workspace won't open
- Binary not found → workspace won't open

**Rationale:** NEEDLE runs autonomously across many workspaces. A capability mismatch could silently break safety guarantees (atomic claims) or cause data loss (non-atomic splits). Failing closed forces operators to resolve the mismatch before workers can proceed.

## Testing Capability Negotiation

When writing tests that mock or fixture the capabilities command, ensure the mock returns all required fields:

```rust
// Example test fixture from src/bead_store/mod.rs:1454-1462
fn version_fixture(directory: &Path, name: &str, version: &str) -> PathBuf {
    std::fs::write(
        &path,
        format!(
            r#"#!/bin/sh
            if [ "$1" = capabilities ]; then
              printf '%s\n' '{{"implementation":"bead-rs","atomic_claim":true,"statuses":["open","in_progress","deferred","closed"],"schemas":[{{"schema_ref":"urn:bead-rs:schema:issue:native-v1"}},{{"schema_ref":"urn:bead-rs:schema:event:native-v1"}},{{"schema_ref":"urn:bead-rs:schema:field-guide:native-v1"}}]}}'
            else
              echo '{version}'
            fi\n"#
        )
    ).unwrap();
    // ... set permissions ...
    path
}
```

**Critical:** The fixture MUST include:
1. `implementation: "bead-rs"`
2. `atomic_claim: true`
3. All four statuses
4. All three schema URNs

## Version Compatibility

Capabilities negotiation is **version-agnostic** — it checks for feature presence, not version numbers. This allows:

- Forward compatibility: New bead-rs versions can add capabilities without breaking NEEDLE
- Backward compatibility: NEEDLE can work with older bead-rs versions as long as they provide required capabilities

**What this means in practice:**
- bead-rs v0.2.0 can add a new `"feature_x": true` field — NEEDLE ignores it
- bead-rs v0.0.1 that lacks `atomic_claim: true` — NEEDLE rejects it, regardless of version

## Related Documentation

- **Backend descriptors:** `src/bead_store/backend.rs` — `BeadBackendCapabilities` struct
- **Runtime verification:** `src/bead_store/mod.rs` — `verify_bead_rs_capabilities()` function
- **Test isolation:** `CLAUDE.md` — "Test Isolation Policy" section
- **Schema compatibility:** bead-rs repository — schema definitions under `urn:bead-rs:schema:*`

## Summary

**The capabilities negotiation contract is:**

1. **Mandatory:** Every bead-rs workspace MUST support `bead capabilities --profile native-v1`
2. **Structured:** The response MUST include `implementation`, `atomic_claim`, `statuses`, and `schemas` fields
3. **Validated:** NEEDLE validates the response at workspace-open time and fails closed on mismatch
4. **Safety-critical:** Missing `atomic_claim: true` breaks multi-worker safety guarantees
5. **Version-agnostic:** Capabilities are checked by feature presence, not version numbers

This contract ensures NEEDLE can safely and reliably interact with bead-rs backends across versions, deployments, and concurrent worker scenarios.
