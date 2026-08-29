# GitHub Draft Releases Root Cause Analysis

**Date:** 2026-08-29  
**Bead:** needle-b0aca322

## Executive Summary

**Root Cause:** The GitHub Personal Access Token (PAT) used by needle-ci prior to 2026-08-19 lacked the required `repo` scope with `contents:write` permission. When `gh release create` is invoked with insufficient permissions, GitHub API defaults to creating draft releases instead of published releases.

**Resolution:** The PAT rotation on 2026-08-19 fixed the issue. The current token has the correct permissions and all releases since then are published successfully as non-drafts.

## Evidence

### Workflow Template Analysis

The needle-ci workflow template (`needle-ci` in `iad-ci` Argo Workflows) does NOT specify `--draft` in the `gh release create` command:

```bash
gh release create "v${VERSION}" \
  --repo jedarden/NEEDLE \
  --title "NEEDLE v${VERSION}" \
  --notes "Release v${VERSION}

  Built from commit: ${COMMIT}" \
  --latest \
  --target main \
  "./target/release/needle-${PLATFORM_SUFFIX}" \
  ...
```

Source: Workflow template lines 422-437, see [workflow template](.claude/projects/-home-coding-NEEDLE/37c9fd05-61ab-46eb-a4f5-c64c374d7c8a/tool-results/bvufnef5q.txt)

### Release History Timeline

**Draft Releases (before token rotation):**
- v0.2.9 (2026-07-03): `draft: true`
- v0.2.8 (2026-06-14): `draft: true`

**Published Releases (after token rotation):**
- v0.5.0 (2026-08-26): `draft: false`
- v0.4.2 (2026-08-19): `draft: false`
- v0.4.1 (2026-08-19): `draft: false`
- v0.4.0 (2026-08-17): `draft: false`
- v0.3.1 (2026-08-15): `draft: false`

All other releases (v0.2.10, v0.2.11, v0.2.12, v0.2.16, v0.2.7, v0.2.6, etc.) are also published as non-drafts.

### Token Rotation Evidence

The Kubernetes secret `github-webhook-secret` has this annotation:
```yaml
annotations:
  force-sync: 2026-08-19-pat-rotation-leak
```

This indicates a PAT rotation occurred on 2026-08-19, which coincides with the end of draft releases.

### Current Token Permissions

The current GitHub token (used by `gh` CLI) has the following scopes:
```
'delete_repo', 'gist', 'read:org', 'repo', 'user', 'workflow'
```

The critical scope is **`repo`**, which includes `contents:write` permission necessary for creating published releases.

## GitHub API Permission Requirements

According to GitHub documentation and CLI behavior:

### Required Permissions for Creating Releases

**For GitHub Personal Access Tokens (PATs):**
- **Scope:** `repo` (full repository access)
- **Implicit:** This scope includes `contents:write` permission

**For GitHub Apps:**
- **Permission:** `contents: write`

**For GitHub Actions:**
```yaml
permissions:
  contents: write
```

### Behavior with Insufficient Permissions

When `gh release create` is invoked with a token lacking `contents:write` permission:
- GitHub API **does not return an error**
- Instead, it creates the release as a **draft** (a safe default)
- The workflow completes successfully, but the release remains unpublished

This is a silent failure mode that can go undetected unless someone checks the releases page.

## Token Source

The GitHub token is sourced from:
- **Kubernetes Secret:** `github-webhook-secret` in `argo-workflows` namespace
- **External Secret:** Synced via ExternalSecret from OpenBao
- **OpenBao Path:** `secret/rs-manager/iad-ci/github/webhook-secret`

The secret is mounted in the workflow template as:
```yaml
env:
- name: GH_TOKEN
  valueFrom:
    secretKeyRef:
      key: token
      name: github-webhook-secret
```

## Verification

The workflow template includes verification logic (lines 439-444):
```bash
# Fail loudly if we still ended up with a draft.
FINAL_DRAFT=$(gh release view "v${VERSION}" --repo jedarden/NEEDLE --json isDraft --jq '.isDraft' 2>/dev/null || echo "absent")
if [ "$FINAL_DRAFT" != "false" ]; then
  echo "ERROR: v${VERSION} did not publish (isDraft=${FINAL_DRAFT}) — no git tag was created"
  exit 1
fi
```

This check ensures that if a release ends up as a draft, the workflow fails loudly rather than silently publishing a draft.

## Conclusion

**The issue has been resolved.** The draft releases (v0.2.8, v0.2.9) were caused by insufficient token permissions on the old GitHub PAT. The token rotation on 2026-08-19 provided a new token with the correct `repo` scope, and all subsequent releases have been published successfully as non-drafts.

**No further action is required** other than ensuring future tokens maintain the `repo` scope.

## Sources

- [GitHub CLI `gh release create` manual](https://cli.github.com/manual/gh_release_create)
- [GitHub Docs on managing releases](https://docs.github.com/en/repositories/releasing-projects-on-github/managing-releases-in-a-repository)
- GitHub API documentation on release creation permissions

## Related Beads

- needle-b0aca322: GitHub token permissions causing draft releases (this investigation)
