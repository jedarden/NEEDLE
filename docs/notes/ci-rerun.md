# Re-running needle-ci for a commit

needle-ci normally starts from a GitHub push webhook (Forgejo -> GitHub mirror
-> `argo-events/github-webhooks` `needle` -> `needle-ci-sensor`). When a run is
lost (reaped as unschedulable, superseded, or the push predates a fix), the
commit has no verdict until the next push or the 6-hourly periodic run. Use the
re-run trigger instead (bead needle-1e9c5d99).

```bash
# One-time: put the token in a mode-600 file (read identity, never printed)
install -d -m 700 ~/.config/needle-ci-rerun
install -m 600 /dev/null ~/.config/needle-ci-rerun/token
bao-as rs-manager bao kv get -field=webhook-secret \
  secret/rs-manager/iad-ci/needle/ci-rerun-webhook > ~/.config/needle-ci-rerun/token

# Re-run CI for a commit on main (full 40-hex SHA)
SHA=$(git rev-parse origin/main)
curl -sS -X POST https://needle-ci-rerun-iad-ci-ts.ardenone.com:8444/needle-rerun \
  -H "Authorization: Bearer $(cat ~/.config/needle-ci-rerun/token)" \
  -H 'Content-Type: application/json' -d "{\"after\":\"$SHA\"}"
```

What it does:
- It submits the same `needle-ci-supersede` pass and `needle-ci` run that a push
  does, with `revision=<SHA>`. The run is labelled
  `ci.ardenone.com/trigger=rerun`. It does not rebuild the CI builder image.
- Find the run with
  `kubectl --server=http://traefik-iad-ci:8001 get workflows -n argo-workflows -l ci.ardenone.com/trigger=rerun`.

Boundaries:
- Tailnet only: the hostname is served on iad-ci Traefik's `vpn` entrypoint
  and has no public record.
- The token is its own credential, stored at OpenBao
  `rs-manager/iad-ci/needle/ci-rerun-webhook`. Requests without it get `401`.
- The sensor accepts only a lowercase 40-hex `after`. Anything else returns
  `200` from the EventSource but starts nothing.
- The caller can name a revision and nothing else. There is no access to the
  Argo API, to templates, or to pod specs.

Manifests (declarative-config):
- `k8s/iad-ci/argo-events/forgejo-eventsource.yml` (route `needle-rerun`)
- `k8s/iad-ci/argo-events/needle-ci-sensor.yml` (dependency and triggers)
- `k8s/iad-ci/argo-events/needle-ci-rerun-externalsecret.yml`
- `k8s/iad-ci/traefik/needle-ci-rerun-ingressroute.yml`
- `k8s/iad-ci/tailnet-external-dns/dnsendpoints.yaml`
- `k8s/iad-ci/argo-events/jetstream-watchdog-needle-ci-deployment.yml` (consumers)
