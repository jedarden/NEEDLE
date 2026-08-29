# Clippy Findings — 2026-08-29

**Command:** `cargo clippy --all-targets -- -D warnings`  
**Result:** ❌ FAILED — 23 errors, 14 clippy warnings  
**Severity:** BLOCKING (compilation errors prevent build)

---

## Summary by Category

| Category | Count | Severity |
|----------|-------|----------|
| Compilation Errors | 23 | 🔴 CRITICAL (build-blocking) |
| Dead Code Warnings | 3 | 🟡 Medium |
| Doc Comment Warnings | 11 | 🟢 Low (style) |

---

## 🔴 CRITICAL: Compilation Errors (Build Blocking)

### File: `tests/panic_safety_verification.rs`

#### Error Type: API Contract Violations (9 errors)

**Missing/Incorrect Methods Called:**

1. **Lines 86, 152, 381, 425, 426, 427**: `track_file()` does not exist on `CleanupGuard`
   ```rust
   guard.track_file(test_path.clone());  // ❌ Method not found
   ```
   - **Impact**: Test cannot compile
   - **Root Cause**: Test code references non-existent API
   - **Fix Required**: Either implement `track_file()` or update test to use correct API

2. **Line 119**: `track_custom_cleanup()` does not exist (should be `track_custom_path`)
   ```rust
   guard.track_custom_cleanup(nonexistent_path.clone(), ...)  // ❌ Wrong method name
   ```
   - **Suggested Fix**: Use `track_custom_path()` instead

#### Error Type: Missing Trait Imports (5 errors)

3. **Line 120**: Missing `anyhow::Context` trait for `.with_context()`
   ```rust
   fs::remove_dir_all(path).with_context(|| format!("failed to remove {:?}", path))
   // ❌ with_context requires trait import
   ```
   - **Fix**: Add `use anyhow::Context;`

4. **Lines 298, 337**: Missing `CommandExt` trait for `.process_group()`
   ```rust
   Command::new("sh")
       .arg("-c")
       .process_group(0)  // ❌ CommandExt trait not in scope
   ```
   - **Fix**: Add `use std::os::unix::process::CommandExt;`

#### Error Type: Type Mismatch (1 error)

5. **Line 233**: `std::process::Child` passed where `tokio::process::Child` expected
   ```rust
   let _guard = ProcessGuard::new(child);  
   // ❌ child is std::process::Child, expected tokio::process::Child
   ```
   - **Root Cause**: Mixing sync and async process types
   - **Fix**: Use `tokio::process::Command` to spawn `tokio::process::Child`

---

## 🟡 Medium: Dead Code Warnings

### File: `src/dispatch/mod.rs`

**Issue**: Test helper functions marked `dead_code` but never used publicly

1. **Line 2805**: `fn test_adapter(name: &str, template: &str) -> AgentAdapter`
   - Defined but never called
   - Likely intended for internal testing

2. **Line 2827**: `fn test_prompt(content: &str) -> BuiltPrompt`
   - Defined but never called
   - Likely intended for internal testing

3. **Line 2838**: `fn test_dispatcher(adapters: HashMap<String, AgentAdapter>) -> Dispatcher`
   - Defined but never called
   - Likely intended for internal testing

**Severity Context**: These are helper functions for the module's own tests. With `-D warnings`, they cause compilation to fail even though they're not production code.

**Fix Options**:
- Mark with `#[expect(dead_code)]` if they're used in test-only code
- Mark with `#[cfg(test)]` if they're test-only
- Remove if truly unused

---

## 🟢 Low: Documentation/Style Warnings

### File: `tests/panic_safety_verification.rs`

**Issue Category 1: Unused Doc Comments (8 warnings)**

Doc comments (`///`) placed on statements instead of declarations:

- **Lines 67-78**: Doc comment before `let mut guard = ...;`
- **Lines 102-113**: Doc comment before `let mut guard = ...;`  
- **Lines 130-141**: Doc comment before `let temp_dir = ...;`
- **Lines 162-173**: Doc comment before `let nonexistent_path = ...;`
- **Lines 184-195**: Doc comment before `let nonexistent_file = ...;`
- **Lines 361-373**: Doc comment before `let mut guard = ...;`
- **Lines 393-405**: Doc comment before `let temp_dir1 = ...;`

**Explanation**: Rustdoc generates documentation for items (functions, structs, etc.), not statements. Doc comments on local variable statements are ignored.

**Fix**: Convert to regular comments (`//`):
```rust
// **Parent Bead AC**: Test edge cases that might trigger panics (e.g., double cleanup)
// 
// This test verifies that calling `cleanup()` twice on the same CleanupGuard
// does not panic. [...]
```

---

**Issue Category 2: Empty Lines After Doc Comments (3 warnings)**

- **Line 221-222**: Empty line after doc comment before `use std::process::Command;`
- **Line 254-255**: Empty line after doc comment before `use std::process::Command;`
- **Line 287-288**: Empty line after doc comment before `use needle::process_guard::ProcessGroupKillGuard;`
- **Line 327-328**: Empty line after doc comment before `use needle::process_guard::ProcessGroupKillGuard;`
- **Line 456-457**: Empty line after doc comment before `use std::io;`

**Explanation**: Clippy's `empty-line-after-doc-comments` lint flags blank lines between doc comments and the item they document when the blank line serves no purpose.

**Fix**: Remove the empty line, or if the comment doesn't apply to the following item, convert to regular comment.

---

## Prioritized Fix Order

### Phase 1: Unblock Build (Required for CI)
1. ✅ Fix `CleanupGuard` API calls (implement or remove)
2. ✅ Fix `track_custom_cleanup` → `track_custom_path`
3. ✅ Add trait imports (`anyhow::Context`, `CommandExt`)
4. ✅ Fix sync/async process type mismatch

### Phase 2: Clear Dead Code Warnings
5. ✅ Mark test helpers with `#[cfg(test)]` or `#[expect(dead_code)]`

### Phase 3: Fix Documentation Style
6. ✅ Convert statement doc comments to regular comments
7. ✅ Remove spurious empty lines after doc comments

---

## File-by-File Breakdown

### `tests/panic_safety_verification.rs`
- **Status**: 🔴 BLOCKING
- **Errors**: 9 compilation errors, 11 style warnings
- **Action Required**: YES — Cannot compile tests
- **Root Cause**: Test code uses non-existent API and missing trait imports
- **Estimated Fix Time**: 30-60 minutes

### `src/dispatch/mod.rs`  
- **Status**: 🟡 WARNINGS
- **Warnings**: 3 dead code warnings
- **Action Required**: YES — Fails CI with `-D warnings`
- **Root Cause**: Test helper functions not marked as test-only
- **Estimated Fix Time**: 5 minutes

---

## Recommendation

**Priority 1**: Fix `tests/panic_safety_verification.rs` compilation errors first. This file cannot compile and blocks the entire test suite.

**Priority 2**: Address dead code warnings in `src/dispatch/mod.rs` to unblock CI.

**Priority 3**: Clean up doc comment style issues (these are cosmetic but will fail CI with `-D warnings`).

**Note**: All findings must be fixed to pass CI with the current strict clippy configuration (`-D warnings`).
