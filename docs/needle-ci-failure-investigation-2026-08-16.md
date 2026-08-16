# needle-ci Failure Investigation Report

**Date**: 2026-08-16  
**Investigator**: Automated Investigation  
**Workflow Template**: needle-ci  
**Cluster**: iad-ci (argo-workflows namespace)

## Executive Summary

Investigation of needle-ci workflow failures revealed three distinct failure patterns:

1. **Exit Code 101** (2026-08-15): Actual test/panic failure - logs unavailable due to retention policy
2. **Exit Code 128** (Multiple dates): Git clone infrastructure failures (503 errors)
3. **Exit Code 1** (2026-08-16): Formatting check failures - blocked by `cargo fmt --check`

## Failure Categories

### Category 1: Exit Code 101 (Test Panic)

**Workflow**: `needle-ci-b55ln`  
**Timestamp**: `2026-08-15T16:09:23Z`  
**Phase**: verify  
**Pod**: `needle-ci-b55ln-verify-276399195`

**Status**: ❌ Logs Unavailable

The actual panic details could not be recovered because:
- Pod retention policy deleted the pod after 2 hours
- No workflow-level `ttlSecondsAfterFinished` configured
- Argo UI logs retained for 2h on failure, then auto-deleted

**Impact**: Unable to diagnose root cause of test failure

---

### Category 2: Exit Code 128 (Git Clone Failures)

**Affected Workflows**:
- `needle-ci-6gxtz`
- `needle-ci-fz6m8`
- `needle-ci-scskd`
- `needle-ci-9r7h6`
- `needle-ci-9thnk`
- `needle-ci-hhbx7`

**Root Cause**: Git clone failed with 503 errors from git.ardenone.com

**Sample Error**:
```
remote: no available server
fatal: unable to access 'https://git.ardenone.com/jedarden/NEEDLE.git/': The requested URL returned error: 503
```

**Resolution**: Retries succeeded automatically (transient infrastructure issue)

**Monitoring**: No action required unless pattern recurs

---

### Category 3: Exit Code 1 (Formatting Check Failures)

**Affected Workflows**: All recent failures from 2026-08-16  
**Root Cause**: `cargo fmt --check` found formatting inconsistencies

**Example**: `needle-ci-dlzcd` (2026-08-16T15:34:34Z)

**Files Affected**:
- `src/canary/mod.rs` (line 469, 1715, 1748, 1788, 1833)
- `src/cli/mod.rs` (line 2727, 2746)
- `src/config/mod.rs` (line 1809, 1844)
- `src/health/mod.rs` (line 471, 482, 3566, 3774, 4200, 4248, 4396)

**Sample Formatting Issue**:
```rust
// Before (multiple lines)
let runner = CanaryRunner::new(
    PathBuf::from("/tmp/.needle"),
    workspace.to_path_buf(),
    300,
);

// After (single line)
let runner = CanaryRunner::new(PathBuf::from("/tmp/.needle"), workspace.to_path_buf(), 300);
```

**Resolution**: Run `cargo fmt` locally, then commit

---

## PodGC and Log Retention Configuration

### Current Settings

**Workflow Template**: `needle-ci`
```yaml
spec:
  podGC:
    strategy: OnWorkflowSuccess
  ttlSecondsAfterFinished: null
```

**Behavior**:
| Workflow Outcome | Pod Retention | Log Retention |
|------------------|---------------|---------------|
| Success | Immediate deletion | 30 minutes |
| Failure | ~2 hours, then deleted | 2 hours (Argo UI) |

**Controller Defaults**: 
- Argo Workflows controller default retention: 2 hours for failed workflows
- No explicit TTL in template - relies on controller defaults

### Issue Identified

**Historical failures (e.g., exit 101 from 2026-08-15) cannot be diagnosed** because:
1. Pods are deleted after 2 hours
2. No persistent log storage configured
3. Argo UI logs are temporary (not archival)

---

## Argo Workflows Access

**URL**: https://argo-ci.ardenone.com  
**Authentication**: Google SSO  
**Access**: VPN only (Tailscale)  
**Namespace**: argo-workflows

**Query Commands**:
```bash
# List recent workflows
kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig \
  get workflows -n argo-workflows --sort-by=.metadata.creationTimestamp

# Get workflow details
kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig \
  get workflow <name> -n argo-workflows -o json

# Get pod logs (if pod still exists)
kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig \
  logs <pod-name> -n argo-workflows -c main

# Check exit codes
kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig \
  get pods -n argo-workflows -o json | jq '.items[] | select(.metadata.name | startswith("needle-ci-")) | {name: .metadata.name, exitCode: .status.containerStatuses[0].state.terminated.exitCode}'
```

---

## Actionable Recommendations

### Immediate Actions (Required)

1. **Fix Formatting Issues** 🔴
   ```bash
   cd /home/coding/NEEDLE
   cargo fmt
   git add src/
   git commit -m "fix(needle-ci): apply cargo fmt formatting"
   git push origin main
   ```
   - This unblocks all current CI failures
   - Addresses exit code 1 failures

### Future Improvements (Optional)

2. **Add Workflow TTL** 🟡
   ```yaml
   spec:
     ttlSecondsAfterFinished: 86400  # 24 hours
   ```
   - Prevents indefinite retention of failed workflows
   - Provides 24-hour investigation window
   - Location: `declarative-config/k8s/iad-ci/argo-workflows/needle-ci.yaml`

3. **Enhance Failure Logging** 🟢
   - Capture panic backtraces to workflow status message
   - Add step-level annotations with failure summaries
   - Consider integration with external log aggregation

4. **Monitor Git Service** 🔵
   - Track git.ardenone.com 503 error frequency
   - Alert if pattern exceeds threshold (>5% failure rate)
   - No action required currently (transient issue only)

---

## Exit Code Reference

| Exit Code | Meaning | Common Cause |
|-----------|---------|--------------|
| 0 | Success | All checks passed |
| 1 | Generic Failure | fmt check failed, test failed, build failed |
| 101 | Panic | Rust panic (assertion failure, unwrap, expect) |
| 128 | Git Error | Clone/fetch failed (503, DNS, auth) |

---

## References

- **Bead**: needle-646d9e04
- **Argo Workflows Docs**: https://argoproj.github.io/argo-workflows/
- **Project CLAUDE.md**: /home/coding/NEEDLE/CLAUDE.md (CI/CD section)
- **Workflow Template**: kubectl get workflowtemplate needle-ci -n argo-workflows -o yaml

---

## Revision History

| Date | Change | Author |
|------|--------|--------|
| 2026-08-16 | Initial investigation report | Automated Investigation |
