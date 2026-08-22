# CI sensor JetStream watchdog protection

**Date**: 2026-08-22
**Deployed via**: `declarative-config` commit `4b9e8fa7` —
`k8s/iad-ci/argo-events/jetstream-watchdog-needle-ci-deployment.yml`

## What this covers

Pushes to NEEDLE `main` reach CI through `needle-ci-sensor` (argo-events,
iad-ci), whose JetStream pull subscription is what turns a Forgejo webhook
into a `needle-ci-*` workflow. On 2026-07-31 a JetStream leadership change
killed that subscription silently: webhooks kept returning 200 OK, the pod
stayed Ready, and every push went unverified for days until someone noticed
(declarative-config bead `declarat-294cc99f`). The `/healthz` probes rolled
out 2026-08-06 do not catch this mode — a wedged sensor still serves
`/healthz`.

As of 2026-08-22 the sensor is protected by a `jetstream-watchdog`
Deployment (`jetstream-watchdog-needle-ci-sensor`, image
`ronaldraygun/jetstream-watchdog:0.5.1`), the second onboarding after the
agentscribe-ci-sensor pilot. It polls the eventbus exporter's consumer
metrics every 15s for both of this sensor's durable consumers
(`group-803207083` = needle-ci-trigger, `group-293399957` =
needle-ci-builder-trigger) and, on detecting a stranded/ack-stalled/missing
consumer while the pod is Ready, deletes the sensor pod and verifies a
fresh Ready replacement — bounded recovery instead of a silent wedge.

## Operator notes

- Watchdog logs: `kubectl -n argo-events logs deployment/jetstream-watchdog-needle-ci-sensor`
- Sensor logs: `kubectl -n argo-events logs -l sensor-name=needle-ci-sensor`
- If a trigger is renamed in the sensor manifest, the durable consumer
  names change (fnv32a of `sensorName-triggerName-depName`) and the
  watchdog's `WATCHDOG_CONSUMER_NAME` list must be recomputed — the
  deployment manifest in declarative-config carries the derivation.
- This protects workflow *creation*. Failures inside a created needle-ci
  workflow are a separate topic — see
  `docs/needle-ci-failure-investigation-2026-08-16.md`.
