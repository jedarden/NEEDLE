# Asynchronous post-push CI

Post-push CI is a separate lifecycle from the local validation gates. Local
gates remain the pre-push worker gate; they do not replace the authoritative
Forgejo/Argo result.

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
