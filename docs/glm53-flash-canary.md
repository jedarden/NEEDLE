# GLM-5.3-Flash productivity canary

The corpus in `canary/glm53-flash/` is a hermetic productivity comparison
fixture. It is intentionally separate from NEEDLE's configured workspace and
from every real bead store. Each task/arm assignment gets a new temporary git
repository, a local JSON fixture store, a temporary `HOME`, and Explore
disabled. The runner refuses a root under `/home/coding`.

Run the offline reference profile and write a sanitized machine-readable
artifact to a temporary location:

```bash
OUT="$(mktemp /tmp/glm53-results.XXXXXX.json)"
python3 scripts/glm53_flash_canary.py --profile good --output "$OUT"
```

The deliberate regression profile is an evaluator test. It must exit non-zero
and produce a result whose quality guardrails report a failed success rate and
unrelated mutations:

```bash
python3 scripts/glm53_flash_canary.py --profile bad --output /tmp/glm53-bad.json
```

The repeatable regression command is:

```bash
canary/glm53-flash/test.sh
```

## Corpus contract

The six pinned fixtures cover a narrow bug fix, multi-file refactor, failing
test diagnosis, documentation-only correction, verification-only no-change
outcome, and shared-checkout safety. Each fixture pins its starting commit in
`corpus.json`; the committed repository also contains the bead description,
`AGENTS.md` guidance, executable `.canary/gate.sh`, expected path policy, and
acceptance evidence. The runner verifies the starting commit before dispatch.

The `reference` and `compact-context` arms have distinct prompt versions,
context modes, and deterministic assignment IDs. The output key for every task
row includes effective model, adapter version, harness version, prompt version,
context mode, and effort.

An `alias-audit` adds two provider-resolution rows. Requested `glm-4.7` and
requested `glm-5.3-flash` must both report effective `glm-5.3-flash`. This is a
provider-resolution identity assertion; stochastic output equality is not used.

## External adapter interface

Use `--agent-command '...'` to run a real adapter in the same isolated fixture.
The command runs with `cwd=$CANARY_REPOSITORY` and receives scalar environment
metadata plus read-only task material at `$CANARY_TASK_SPEC`. It can close the
fixture bead through the fixture-only `bead` shim. If it writes JSON to
`$CANARY_METRICS_FILE`, the evaluator accepts only numeric usage fields such as
`tool_calls`, `input_tokens`, `output_tokens`, `cache_read_tokens`,
`cache_write_tokens`, `credits`, and categorical flags for `provider_error`,
`max_turns`, `duplicate_claim`, or `reaped`.

Agent output, prompt bodies, credentials, and reasoning traces are discarded
and never copied into the result artifact. A real adapter should report its
served identity as `effective_model`; the runner retains the requested model
separately.

## Scoring and failure domains

`verified_success` requires a closed fixture bead, a passing gate, and no
unsafe or unrelated file mutation. A close that fails those checks is reopened
and counted as `false_close_reopens`. Duplicate claims and reaped/stale-release
attempts are reported in `control_plane_health` and excluded from model
productivity denominators. Provider errors have their own rate and are not
silently converted to model failures. The artifact reports attempts per
verified close, timeout/max-turns/provider-error rates, wall time, tool calls,
tokens, cache reads/writes, credits per verified close, and mutation counts.

The corpus has 12 task-arm observations per run (six tasks × two arms), plus
two alias-audit observations. This is a small, bounded sample: the rates are
descriptive, not population estimates, and the fixture tasks are not a random
sample of production work. Repeat independent runs and report uncertainty or
confidence intervals before making a fleet-wide model decision. Provider
outages, adapter changes, harness changes, and control-plane incidents should
be reported separately from model quality.
