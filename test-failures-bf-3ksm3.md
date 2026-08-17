# Integration Test Failures - bf-3ksm3

**Test Run Date:** 2026-07-15  
**Total Tests:** 1417  
**Passed:** 1416  
**Failed:** 1  
**Ignored:** 0  
**Test Duration:** 632.66 seconds (~10.5 minutes)

## Failing Test

### Test Name
`cli::tests::find_all_descendants_handles_cycles`

### Location
`src/cli/mod.rs:5755:9`

### Failure Details
```
thread 'cli::tests::find_all_descendants_handles_cycles' (191564) panicked at src/cli/mod.rs:5755:9:
assertion `left == right` failed
  left: 2
 right: 1
```

### Test Code (lines 5740-5757)
```rust
#[test]
fn find_all_descendants_handles_cycles() {
    // Test that the visited set prevents infinite loops
    use std::collections::{HashMap, HashSet};

    // Create a cycle: 1 -> [2], 2 -> [1]
    let mut ppid_to_children: HashMap<u32, Vec<u32>> = HashMap::new();
    ppid_to_children.insert(1, vec![2]);
    ppid_to_children.insert(2, vec![1]);

    let mut descendants = Vec::new();
    let mut visited = HashSet::new();

    // This should not loop infinitely
    find_descendants_recursive(1, &ppid_to_children, &mut descendants, &mut visited);

    // Should find 2, then stop when it encounters 1 again (already visited)
    assert_eq!(descendants.len(), 1);  // <-- FAILS HERE: got 2, expected 1
    assert!(descendants.contains(&2));
}
```

### Function Implementation (lines 1148-1162)
```rust
fn find_descendants_recursive(
    pid: u32,
    ppid_to_children: &HashMap<u32, Vec<u32>>,
    descendants: &mut Vec<u32>,
    visited: &mut HashSet<u32>,
) {
    if let Some(children) = ppid_to_children.get(&pid) {
        for &child_pid in children {
            if visited.insert(child_pid) {
                descendants.push(child_pid);
                find_descendants_recursive(child_pid, ppid_to_children, descendants, visited);
            }
        }
    }
}
```

### Root Cause Analysis

**Bug:** The initial PID is never added to the visited set before recursion begins.

**Execution trace with cycle 1 -> [2], 2 -> [1]:**
1. `find_descendants_recursive(1, ...)` called
2. Look up PID 1 → children = [2]
3. Child 2: `visited.insert(2)` succeeds → add 2 to descendants → recurse
4. `find_descendants_recursive(2, ...)` called  
5. Look up PID 2 → children = [1]
6. Child 1: `visited.insert(1)` **succeeds** (PID 1 was never marked as visited!) → add 1 to descendants → recurse
7. `find_descendants_recursive(1, ...)` called again
8. Look up PID 1 → children = [2]
9. Child 2: `visited.insert(2)` fails (already visited) → stop
10. **Result:** descendants = [2, 1] (length 2, not 1)

**Expected behavior:** Only PID 2 should be in descendants (length 1), and the cycle back to PID 1 should be detected.

### Fix Required

Mark the initial PID as visited before starting recursion:

```rust
// Before calling find_descendants_recursive, add:
visited.insert(pid);
```

Or modify the function to add the initial PID to visited at the start:

```rust
fn find_descendants_recursive(
    pid: u32,
    ppid_to_children: &HashMap<u32, Vec<u32>>,
    descendants: &mut Vec<u32>,
    visited: &mut HashSet<u32>,
) {
    visited.insert(pid);  // Mark current PID as visited to prevent cycles
    if let Some(children) = ppid_to_children.get(&pid) {
        for &child_pid in children {
            if visited.insert(child_pid) {
                descendants.push(child_pid);
                find_descendants_recursive(child_pid, ppid_to_children, descendants, visited);
            }
        }
    }
}
```

### Impact Assessment

- **Severity:** Medium (logic error in cycle detection)
- **Scope:** Affects `find_all_descendants` and any code that relies on it for process tree traversal
- **Risk:** Could cause infinite loops in production if process tree cycles exist in rare scenarios
- **User impact:** Currently low - cycles are rare in real process trees, but the bug should be fixed

## Test Environment

- **Platform:** Linux (Hetzner EX44)
- **Rust toolchain:** stable-x86_64-unknown-linux-gnu
- **Test command:** `cargo test --lib`
- **Exit code:** 1 (failure)

## Additional Notes

The test suite also produced warnings about NEEDLE workers being stopped unexpectedly, but these appear to be related to the test environment setup and not test failures per se:

```
NEEDLE worker 'test-worker' stopped unexpectedly: state=Selecting, beads_processed=0, uptime=18s
This indicates the worker was killed by an external process (e.g., SIGKILL, OOM, capacity governor)
Heartbeat file already removed: /home/coding/.needle/state/heartbeats/claude-test-worker.json
```

These warnings appeared multiple times during the test run but did not cause test failures.