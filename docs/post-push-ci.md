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
  minutes, not seconds — observed ~3 min on one push, and on another it never
  fired at all: commits confirmed on GitHub 86 min after push, no workflow
  for that revision (2026-10-23). A 2-minute wait finds nothing, and even
  25 min can miss.
- **Reads are credential-free:** `kubectl --server=http://traefik-iad-ci:8001`
  reads workflows, pods, and logs. The `~/.kube/iad-ci.kubeconfig` auth is
  dead (Unauthorized) and cannot even read — do not use it.
- **Failed runs keep their pods:** the `needle-ci` WorkflowTemplate overrides
  the controller-default `podGC: OnPodCompletion` with
  `podGC: OnWorkflowSuccess`. A failed run's verify/shard pods stay up with
  full logs — read them instead of streaming while running or re-submitting
  (something outside podGC reaps the pods eventually, so fetch promptly).
  Templates without the override really do lose pods at pod completion.
  Re-confirmed end-to-end on the failed run `needle-ci-q28lt` (2026-10-23):
  all 11 pods retained 20+ min after failure, logs fetched with the
  commands below.
- **Workflow objects are TTL-reaped** — the `needle-ci` template declares
  `ttlStrategy: 1 h after success, 48 h after failure` (the controller
  default applied to other templates is 5 min / 30 min) — and pods are
  reaped eventually, so capture the phase and fetch logs promptly.
- **"Running" can be queue wait:** runs serialize on a ConfigMap lock of 1;
  a workflow can sit in Running for an hour or more with no progress. Check
  the pending node's message before diagnosing a stall.
- **Attribute by revision, not time alone:** a workflow that appears quickly
  may belong to an older queued push, and periodic
  `needle-ci-periodic-*` runs carry `revision=main` — match
  `.spec.arguments.parameters[?(@.name=="revision")]` against your pushed SHA.

```bash
# Record push time and SHA, then poll for the triggered workflow
PUSH_TIME=$(date -u +%Y-%m-%dT%H:%M:%SZ)
SHA=$(git rev-parse HEAD)
git push origin main

# Wait up to 40 min for the workflow to appear (mirror propagation is minutes,
# with real variance: ~3 min on one push 2026-10-23; another push never
# triggered at all).
# If the loop exhausts empty, poll again later — mirror lag is not a green CI.
WF=""
for i in $(seq 120); do
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

## Manual WorkflowTemplate submission — blocked without a live kubeconfig

The environment guide's "Submitting a workflow manually" recipe creates a
Workflow through `~/.kube/iad-ci.kubeconfig`. That kubeconfig's bearer
token expires repeatedly — dead from 2026-09-24, and again since
2026-10-22 (`kubeconfig-expiry-sweep` lists `iad-ci.kubeconfig` as
expired). Verified live 2026-10-23: it cannot even read, and the
credential-free endpoint cannot fill the gap because the proxy's RBAC is
read-only (`auth can-i create workflows.argoproj.io` → `no`).

Failure signature of the dead-kubeconfig recipe — recognize it, don't
retry it: repeated `memcache.go:265] "Unhandled Error" err="couldn't get
current server API group list: the server has asked for the client to
provide credentials"` warnings, ending in `error: You must be logged in
to the server (the server has asked for the client to provide
credentials)`, exit 1.

Pre-flight before any manual submission:

```bash
kubectl --kubeconfig=/home/coding/.kube/iad-ci.kubeconfig \
  auth can-i create workflows.argoproj.io -n argo-workflows   # need: yes
```

`no` — or the Unauthorized signature above — means manual submission from
codinghome is blocked. Unblock paths: an operator re-issues the kubeconfig
(check `~/.local/state/kubeconfig-expiry-sweep/last-report.json`), or a
human submits from the Argo UI (`https://argo-ci.ardenone.com`, Google
SSO, VPN only). Report the task blocked with that unblock path; do not
loop on the dead recipe.

There is no status fallback: neither Forgejo nor the GitHub mirror
carries commit statuses for NEEDLE (`total_count: 0` on both, checked
2026-10-23). The durable, credential-free records of a run are the
CI-check bead, the `.needle/ci/lifecycle.jsonl` ledger, and — until TTL
reaping — the workflow object and retained pods read through the endpoint
above.

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
