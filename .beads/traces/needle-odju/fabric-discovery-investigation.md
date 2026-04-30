# FABRIC DirectoryTailer File Discovery Investigation

## Investigation Questions

1. Does DirectoryTailer add a file only when it has content (first line)?
2. What filename pattern does it match?
3. Could the naming convention mismatch prevent discovery?

## Findings

### 1. File Discovery: Filename Pattern Matching

**Location:** `/home/coding/FABRIC/src/directoryTailer.ts:103`

```typescript
if (!entry.endsWith('.jsonl')) continue;
```

**Answer:** Files MUST end with `.jsonl` exactly. The prefix pattern (e.g., `claude-code-glm-4_7-`) does not matter - only the suffix is checked.

**Files using pattern:** `claude-code-glm-4_7-<session>.jsonl` will be discovered correctly.

### 2. 0-Byte File Handling

**Discovery (lines 102-120):** ALL files ending in `.jsonl` are registered in `fileInfo` regardless of size:
```typescript
this.fileInfo.set(fullPath, {
  mtime: stat.mtimeMs,
  position: stat.size,  // For 0-byte files: position = 0
  lastActivity: 0,
});
```

**Activation (lines 114-126):** Files are only activated as candidates if:
- `now - stat.mtimeMs <= recentMtimeMs` (default 24 hours)
- They are among the `maxActiveFiles` most recently modified files

**Conclusion:** 0-byte files ARE discovered and registered. They will be activated if recent enough and within the active file cap.

### 3. Content Parsing for Empty Files

**Location:** `/home/coding/FABRIC/src/normalizer.ts:154-163`

```typescript
if (typeof raw === 'string') {
  if (!raw || !raw.trim()) return null;  // Empty/whitespace lines return null
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
}
```

**Answer:** Empty lines return `null` gracefully. No error is thrown. A 0-byte file produces no events but is otherwise handled correctly.

## Potential Issues

### Issue 1: Activation Window (Most Likely Root Cause)

**Symptom:** Worker files exist but FABRIC shows no workers.

**Explanation:** The `recentMtimeMs` default is 24 hours. If a worker file was created more than 24 hours before FABRIC starts, it will be:
1. Registered in `fileInfo` ✓
2. Marked as inactive (not a candidate) ✗
3. Only activated when `pollInactiveFiles()` detects an mtime change

**Fix Options:**
1. Increase `recentMtimeMs` on startup
2. Touch/modify the file to update mtime
3. Check if `inactiveCheckIntervalMs` (default 30s) has elapsed since startup

### Issue 2: File Pattern Mismatch (Unlikely)

The pattern check is simple: `entry.endsWith('.jsonl')`. If files use a different extension (e.g., `.json`, `.log`), they will be silently skipped.

**Verify:** Check actual file extensions in the telemetry directory.

### Issue 3: Directory Watcher Latency

New file detection uses a 50ms delay (line 136) to avoid races with file creation. For very rapid file creation/deletion cycles, the file might be missed.

## Recommendations

1. **Check file extensions:** Verify all worker files use `.jsonl` extension
2. **Check mtime:** Run `ls -l` on telemetry files to see modification times
3. **Increase recentMtimeMs:** Consider extending the activation window if files are older
4. **Add debug logging:** Log discovered files and activation decisions to diagnose issues
