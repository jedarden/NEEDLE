# GLM-5.3-Flash Claude Code profiles

The source of truth for the managed Z.AI adapter profiles is
`fleet/ex44/adapters/`. The files are reviewable inputs to NEEDLE; they are
not claims that renaming a retired provider alias changes the model served by
the proxy.

## Profiles and rollout boundary

| Adapter | Context | Effort | Purpose |
|---|---:|---:|---|
| `claude-code-glm-5.3-flash` | standard | `max` | Baseline profile for complex coding |
| `claude-code-glm-5.3-flash-1m` | 1M | `max` | Opt-in canary; requests `glm-5.3-flash[1m]` and `--autocompact 1m` |
| `claude-code-glm-5.3-flash-low` | standard | `low` | Separately named low-effort canary |
| `claude-code-glm-5.3-flash-high` | standard | `high` | Separately named high-effort canary |

The baseline remains standard-context and uses maximum effort explicitly. The
1M, low, and high variants are never selected by changing a hidden setting in
the baseline. A fleet default change requires a separate rollout bead with
canary evidence.

All four profiles use Claude Code stream JSON with
`--include-partial-messages`; this preserves liveness updates and tool events.
They do not append `| cat`, so the dispatcher's `bash -c` observes Claude's
real exit status. The profile validator rejects model/context/effort drift,
thinking-disabled requests, literal auth values, and missing stream settings.
Claude Code `2.1.283` is the declared minimum because that is the installed
version exercised with `--effort`, `--autocompact`,
`--exclude-dynamic-system-prompt-sections`, and
`--no-session-persistence`.

The 1M canary carries the two evaluation flags deliberately:

- `--exclude-dynamic-system-prompt-sections` is an experiment for improving
  prompt-cache reuse by keeping dynamic system sections out of the cacheable
  prefix.
- `--no-session-persistence` matches NEEDLE's one-bead/one-session lifecycle;
  it prevents a later bead from resuming state from this attempt.

The profiles use `--effort max` rather than a thinking-disabled option. This
is the Claude Code-side translation that keeps thinking enabled for the
Anthropic-compatible request. The low-effort canary is intentionally marked
experimental: smoke it through the proxy before using it broadly because the
provider rejects requests whose translated thinking block is disabled or
empty.

## Install and validate

The idempotent installer reads `agent.adapters_dir` from `.needle.yaml` and
installs the four source-controlled files there. A changed destination is
saved as a timestamped `.bak-*` file before replacement; an unchanged file is
left alone.

```text
scripts/install-needle-adapters.sh --dry-run
scripts/install-needle-adapters.sh
needle test-agent claude-code-glm-5.3-flash
```

Use `--adapters-dir` for a hermetic staging directory or `--names` to install
one profile. Endpoint and authentication are supplied by the host environment
(`NEEDLE_ZAI_BASE_URL` and `NEEDLE_ZAI_AUTH_TOKEN`); no credential belongs in
the YAML or in this documentation.

The credential-safe proxy smoke is opt-in and reports only the provider model
identity and sanitized status information:

```text
scripts/smoke-glm53-flash.sh
scripts/smoke-glm53-flash.sh 1m
```

The expected effective model family is `glm-5.3-flash`. A successful smoke is
evidence for the selected profile and proxy path, not evidence that every
provider-side alias is served differently.

Each attempt records the profile version, context mode, effort, maximum turns,
and effective timeout policy in `attempt.resolved`. This makes canary results
comparable even when the selected adapter changes later.

## Rollback

Stop or drain only the affected workers at a bead boundary. Do not remove the
canary files: leaving them installed makes rollback reversible and does not
select them. Restore the previous explicit Flash profile from the backup made
by the installer:

```bash
ADAPTER_DIR=/home/coding/.config/needle/adapters
PREVIOUS=$(find "$ADAPTER_DIR" -maxdepth 1 -type f \
  -name 'claude-code-glm-5.3-flash.yaml.bak-*' -printf '%T@ %p\n' \
  | sort -nr | sed -n '1s/^[^ ]* //p')
test -n "$PREVIOUS" && test -f "$PREVIOUS"
install -m 644 "$PREVIOUS" \
  "$ADAPTER_DIR/claude-code-glm-5.3-flash.yaml"
needle test-agent claude-code-glm-5.3-flash
```

The restored file is the prior explicit `glm-5.3-flash` profile, not the
retired `glm-4.7` alias. If no backup exists, restore the last known-good
profile from the Forgejo revision used for the deployment, then run the same
`needle test-agent` check. Re-promoting the managed source requires a new
install and should be accompanied by the rollout bead's evidence.
