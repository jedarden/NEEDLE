# Asynchronous post-push CI

Post-push CI is a separate lifecycle from the local validation gates. Local
gates remain the pre-push worker gate; they do not replace the authoritative
Forgejo/Argo result.

## Manual runbook — verifying a push on iad-ci

The automated reconciler below is the normal path. When a push needs to be
verified by hand, use this runbook. It mirrors the recipe in
[`NEEDLE/CLAUDE.md`](../CLAUDE.md) ("Do not run `cargo test` locally") —
update both together.

Facts it relies on (verified against the live cluster 2026-10-23):

- **Trigger chain:** Forgejo push → Forgejo's server-side GitHub push mirror
  → GitHub webhook → `needle-ci` WorkflowTemplate. The mirror leg takes
  minutes, not seconds (~8 min observed 2026-10-23, up to ~15), so a
  2-minute wait finds nothing.
- **Reads are credential-free:** `kubectl --server=http://traefik-iad-ci:8001`
  reads workflows, pods, and logs. The `~/.kube/iad-ci.kubeconfig` auth is
  dead (Unauthorized) and cannot even read — do not use it.
- **Failed runs keep their pods:** the `needle-ci` WorkflowTemplate overrides
  the controller-default `podGC: OnPodCompletion` with
  `podGC: OnWorkflowSuccess`. A failed run's verify/shard pods stay up with
  full logs — read them instead of streaming while running or re-submitting.
  Templates without the override really do lose pods at pod completion.
- **Workflow objects are TTL-reaped** (about 5 min after success, 30 min
  after failure) and pods are reaped eventually, so capture the phase and
  fetch logs promptly.
- **"Running" can be queue wait:** runs serialize on a ConfigMap lock of 1;
  a workflow can sit in Running for an hour or more with no progress. Check
  the pending node's message before diagnosing a stall.
- **Attribute by revision, not time alone:** a workflow that appears quickly
  may belong to an older queued push — match
  `.spec.arguments.parameters[?(@.name=="revision")]` against your pushed SHA.

```bash
# Record push time and SHA, then poll for the triggered workflow
PUSH_TIME=$(date -u +%Y-%m-%dT%H:%M:%SZ)
SHA=$(git rev-parse HEAD)
git push origin main

# Wait up to 25 min for the workflow to appear (mirror propagation is minutes)
WF=""
for i in $(seq 75); do
  sleep 20
  WF=$(kubectl --server=http://traefik-iad-ci:8001 -n argo-workflows \
    get workflows --sort-by=.metadata.creationTimestamp \
    -o jsonpath='{range .items[*]}{.metadata.name} {.metadata.creationTimestamp} {.spec.arguments.parameters[?(@.name=="revision")].value}{"\n"}{end}' \
    2>/dev/null | awk -v t="$PUSH_TIME" -v sha="$SHA" '$2 >= t && $1 ~ /^needle-ci-/ && $3 == sha {print $1}' | tail -1)
  [[ -n "$WF" ]] && break
done
echo "Workflow: $WF"

# Poll until done (Running can be lock queue wait; objects are TTL-reaped)
for i in $(seq 60); do
  PHASE=$(kubectl --server=http://traefik-iad-ci:8001 -n argo-workflows \
    get workflow "$WF" -o jsonpath='{.status.phase}' 2>/dev/null)
  echo "[$i] $WF phase=$PHASE"
  if [[ "$PHASE" == "Succeeded" || "$PHASE" == "Failed" || "$PHASE" == "Error" ]]; then break; fi
  sleep 30
done

# On failure, fetch logs from the retained pods (podGC: OnWorkflowSuccess)
if [[ "$PHASE" != "Succeeded" ]]; then
  kubectl --server=http://traefik-iad-ci:8001 -n argo-workflows \
    get pods -l workflows.argoproj.io/workflow="$WF" \
    -o custom-columns='NAME:.metadata.name,PHASE:.status.phase'
  for POD in $(kubectl --server=http://traefik-iad-ci:8001 -n argo-workflows \
    get pods -l workflows.argoproj.io/workflow="$WF" \
    -o jsonpath='{range .items[?(@.status.phase=="Failed")]}{.metadata.name}{"\n"}{end}'); do
    kubectl --server=http://traefik-iad-ci:8001 -n argo-workflows \
      logs "$POD" -c main --tail=200
  done
fi
```

Enable it in global or repository `.needle.yaml` configuration:

```yaml
post_push_ci:
  enabled: true
  default_workflow: needle-ci
  result_url_template: https://argo.example/api/runs/{repository}/{sha}/{workflow}
  auth_token_env: FORGEJO_ARGO_READ_TOKEN
  timeout_secs: 3600
  max_retries: 3
  poll_interval_secs: 30
  repositories:
    https://git.example/acme/widget:
      workflow: widget-authoritative-ci
      timeout_secs: 1800
      max_retries: 5
```

The endpoint must return JSON with a common status field. Forgejo check-run
shapes (`conclusion`, `state`) and Argo Workflow shapes (`status.phase`) are
accepted. `success`/`succeeded` closes the CI-check bead and, if the parent has
no other unfinished blockers, closes the implementation parent. Product/test
failures close the check and create one deduplicated repair bead; the repair
bead blocks the implementation parent. Infrastructure failures and timeouts
append evidence and schedule another poll without creating a code defect.

After a worker pushes an implementation commit, NEEDLE records the normalized
origin, commit SHA, and workflow in `.needle/ci/lifecycle.jsonl`, creates one
check bead, and links it as `check blocks parent`. The worker then releases its
claim. The ledger is append-only and stores only bounded summaries and
credential-free run/log references, so it remains useful after Argo pods and
their logs expire.

## Release quickstart gate

The `needle-ci` WorkflowTemplate runs the `needle-quickstart-gate` step after
each newly published release. It starts in the pinned
`debian:13.1-slim@sha256:a347fd7510ee31a84387619a492ad6c8eb0af2f2682b916ff3e643eb076f925a`
container with no Rust toolchain or bead binary, downloads that release's
published `install.sh` asset, and executes the README quickstart against a
disposable workspace. The release also carries the compatibility-pinned bead
backend built from the same claim-epoch-capable source revision as the CI
builder; this avoids pairing a current NEEDLE binary with the older public
bead-rs v0.2.6 asset:

```text
needle init --backend bead-rs
bead init --prefix gate
bead create --title "Quickstart gate fixture" --priority 2 --issue-type task
needle doctor
needle run --agent noop --identifier gate --timeout 60
bead list --status closed --json
```

The gate requires `needle doctor` to exit zero and contain no `[FAIL]` rows,
and requires the created bead to appear with `status: "closed"`. Its failure
is a normal `needle-ci` failure and blocks a release from appearing healthy.
The `noop` fixture is stored in
[`examples/adapters/noop.yaml`](../examples/adapters/noop.yaml).

The template is deployed by ArgoCD application
`argo-workflows-ns-iad-ci`; inspect that Application after pushing the
declarative-config change. The negative scratch proof deliberately removed the
`install_bead` call from `install.sh`; `needle-quickstart-gate` then failed at
the backend/doctor assertions as intended.

Every implementation commit must have exactly one `Bead-Id: <parent>` trailer.
Missing, duplicate, or mismatched trailers fail closed: the worker releases the
parent without guessing a CI owner. Duplicate registration, webhook delivery,
polls, and repair creation are keyed by repository + SHA + workflow and are
safe to repeat. An old SHA/event cannot advance a newer check.

Run the reconciler independently of coding workers:

```text
needle ci-reconcile --workspace /path/to/repository
needle ci-reconcile --workspace /path/to/repository --once
```

`--once` is useful for an operator recovery cycle. On restart, the reconciler
scans check markers in the bead store and rehydrates missing ledger records,
then bounded polling resumes from the persisted retry deadline and evidence.
If a ledger record says `success_observed` but the process stopped before the
close calls, rerunning the reconciler safely repeats the idempotent close path.

Operator recovery:

1. Confirm the repository's workflow identity and result endpoint, and ensure
   the read-only token is present only in the configured environment variable.
2. Run `needle ci-reconcile --once` and inspect the CI-check bead plus
   `.needle/ci/lifecycle.jsonl`; do not edit the ledger by hand.
3. If the authoritative run is product-failed, work the generated repair bead.
   The next pushed SHA starts a new check key and leaves the old evidence intact.
4. If the run is infrastructure-failed or timed out, restore the endpoint or
   Argo runner and rerun the reconciler. Do not create a code repair bead for
   an infrastructure result.
5. For a missing/ambiguous trailer, add the correct trailer in a new commit
   associated with the implementation bead, push it, and let reconciliation
   begin again. Never manually attach a check to an uncertain parent.
