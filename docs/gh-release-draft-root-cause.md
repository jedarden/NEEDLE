# GitHub Release Draft Root Cause Analysis

## Issue Summary

**Issue:** `gh release create` was producing draft releases instead of published releases in the needle-ci workflow.

**Impact:** Draft releases are invisible to the GitHub `releases/latest` API, breaking `needle upgrade --check` and user installation workflows.

## Root Cause

The GitHub App installation token used by needle-ci lacked the specific permission required to **publish** releases. When `gh release create` was invoked without the `--latest` flag, the GitHub CLI fell back to creating draft releases instead of failing with a clear permission error.

### Technical Details

**Token Used:** GitHub App installation token from `github-webhook-secret` (iad-ci cluster)

**Token Permissions (Current):**
- Repository: `admin`, `maintain`, `push`, `triage`, `pull` ✅
- **Missing:** Explicit release publication permission

**GitHub CLI Behavior:**
- Without `--latest`: Silently falls back to draft creation if publication permission is lacking
- With `--latest`: Forces publication and fails explicitly if permission is insufficient

## Evidence

### Timeline

**Draft Releases (Before Fix):**
- v0.2.8: Created 2026-06-14, `draft: true`
- v0.2.9: Created 2026-07-03, `draft: true`

**Published Releases (After Fix):**
- v0.4.0: Created 2026-08-17, `draft: false` ✅
- v0.4.1: Created 2026-08-19, `draft: false` ✅
- v0.4.2: Created 2026-08-19, `draft: false` ✅
- v0.5.0: Created 2026-08-26, `draft: false` ✅

### Fix Applied

**Commit:** `678e9bd9` (2026-08-15)
**Repository:** `jedarden/declarative-config`
**File:** `k8s/iad-ci/argo-workflows/needle-workflowtemplate.yml`

**Change:** Added `--latest` flag to `gh release create` command

```bash
gh release create "v${VERSION}" \
  --repo jedarden/NEEDLE \
  --title "NEEDLE v${VERSION}" \
  --notes "Release v${VERSION}

Built from commit: ${COMMIT}" \
  --latest \                      # ← ADDED THIS FLAG
  --target main \
  "./target/release/needle-${PLATFORM_SUFFIX}" \
  ...
```

### Commit Message

```
fix(needle-ci): add --latest flag to gh release create to prevent silent draft degradation

The GitHub App token lacks permission to publish releases, causing gh to
degrade to creating drafts instead of failing. Drafts are invisible to
the releases/latest API, breaking 'needle upgrade --check'.

This adds --latest to force publication, with existing post-create validation
that fails if the release is still a draft.

Fixes needle-208840e4
```

## Required Permissions

For a GitHub App token to create **published** (non-draft) releases:

### Minimal Required Scope
- Repository: **Contents: Write** (`contents:write`)
- OR Repository: **Admin** (`admin`) permission

### Current Token Status
- ✅ Has `admin` permission on `jedarden/NEEDLE`
- ⚠️  The `--latest` flag is still required to force publication despite having admin permissions
- This suggests the GitHub App installation may have had misconfigured permissions at the App level

### GitHub App vs. PAT Behavior

| Token Type | Default Behavior | `--latest` Flag Impact |
|------------|------------------|------------------------|
| PAT (Classic) | Publishes directly if permissions allow | Forces latest release flag |
| GitHub App | Falls back to draft if publication permission unclear | Forces publication, fails explicitly if insufficient |

## Current State

**Status:** ✅ **FIXED**

The needle-ci workflow now correctly creates published releases:

1. **Workflow includes `--latest` flag** (line 508 of needle-workflowtemplate.yml)
2. **Post-creation validation** (lines 519-524) fails if release is still a draft
3. **Releases/latest API verification** (lines 526-539) confirms public visibility
4. **All releases since 2026-08-17 are published** (v0.4.0 through v0.5.0)

## Legacy Cleanup

**Outstanding Draft Releases:**
- v0.2.8 (2026-06-14) - draft
- v0.2.9 (2026-07-03) - draft

**Recommended Action:** These can be deleted or published in place. They predate the fix and are no longer relevant.

## Verification Commands

```bash
# Check if latest release is a draft
GH_TOKEN="<token>"
curl -fsSL -H "Authorization: Bearer $GH_TOKEN" \
  "https://api.github.com/repos/jedarden/NEEDLE/releases/latest" | \
  jq -r '{tag_name, draft, published_at}'

# List all releases with draft status
curl -fsSL -H "Authorization: Bearer $GH_TOKEN" \
  "https://api.github.com/repos/jedarden/NEEDLE/releases" | \
  jq -r '.[] | select(.draft == true) | {tag_name, created_at}'

# Verify workflow has --latest flag
grep -A 5 "gh release create" \
  ~/declarative-config/k8s/iad-ci/argo-workflows/needle-workflowtemplate.yml
```

## Lessons Learned

1. **Silent Degradation:** GitHub CLI falls back to draft creation rather than failing explicitly when publication permissions are ambiguous
2. **API Visibility:** Draft releases don't appear in `releases/latest` endpoint, breaking automated version checks
3. **Force Publication:** The `--latest` flag is necessary to enforce publication behavior and fail fast on permission issues
4. **Validation Layers:** Multiple validation checks (post-create, API visibility) are critical for CI/CD reliability

## Related Beads

- needle-208840e4: Original issue tracking silent draft degradation
- needle-7b64771e: Root cause analysis investigation (this document)
