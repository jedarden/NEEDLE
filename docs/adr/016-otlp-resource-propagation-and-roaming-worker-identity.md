# ADR-016: OTLP Resource Propagation and Roaming-Worker Identity

**Status:** Proposed — 2026-08-16
**Deciders:** operator (jedarden)
**Tracking:** plan.md §17; supersedes the incomplete fix closed under `needle-501aa991`

## Context

The NEEDLE dashboard (`dashboard.ardenone.com/needle/`, served by the
`needle-dashboard` service in `ardenone-cluster`) renders a worker card with five
facts: **Environment**, **Harness**, **Model**, **Repo**, and a semver badge. Four of
the five render as `Unknown` for most workers, and **Repo and semver render as
`Unknown` for every worker, always**.

### Evidence 1 — live snapshot, 2026-08-16 04:40 UTC

Fetched from the dashboard's own normalized contract endpoint
(`/api/dashboard` via the `needle-feed` Traefik entrypoint):

```json
{"worker_id": "claude-code-glm-4.7-tradegraph-1", "environment": "ex44",
 "worker_pool": "default", "repo": null, "model": null, "harness": null,
 "semver": null, "state": "idle"}
{"worker_id": "claude-code-glm-4.7-armor-1", "environment": "ex44",
 "worker_pool": "default", "repo": null, "model": "glm-4.7",
 "harness": "claude-code-glm-4.7", "semver": null, "state": "idle"}
```

Across the whole fleet: `repo` is `null` for **every** worker, `semver` is `null`
for **every** worker, `worker_pool` is `"default"` for **every** worker despite
`needle.worker.pool=bare-metal` being configured, and `harness`/`model` are
populated only for the subset of workers that happen to still have a dispatch
event inside the server's ring buffer. `environment` is the one field that
always works — and it is the one field NEEDLE does not supply, because the
collector's `resource/ex44` processor upserts `deployment.cluster` itself.

That asymmetry is the whole diagnosis in miniature: **collector-injected resource
attributes arrive; NEEDLE-supplied resource attributes do not.**

### Evidence 2 — the wire

Captured directly from the deployed `needle` 0.3.1 binary by running one isolated
worker (throwaway `HOME`, throwaway workspace, `compression: none`) against a
local OTLP receiver and dumping the raw `/v1/logs` protobuf. Every string in the
598-byte payload:

```
needle | INFO | {"version":"0.3.1","worker_name":"otelprobe"}
event_type | worker.booting | worker_id | claude-code-glm-4.7-otelprobe
session_id | de7fb055 | sequence
event_type | init.step.started | ... | duration_ms
```

There are **no resource attributes on the wire at all**. Not `service.name`, not
`service.version`, not `deployment.cluster`, not `needle.worker.pool`, and not
the `needle.agent`/`needle.model`/`needle.workspace` trio. The dashboard is not
misreading the payload; the payload is empty of resource.

### Root cause A — the resilient exporter wrappers swallow `set_resource`

`opentelemetry_sdk` 0.31 propagates the Resource by *pushing* it down the chain
at provider-build time:

- `SdkLoggerProvider::build()` → `processor.set_resource(&resource)`
  (`logs/logger_provider.rs:267`)
- `BatchLogProcessor::set_resource()` → `BatchMessage::SetResource`
  (`logs/batch_log_processor.rs:323`)
- the batch worker → `exporter.set_resource(&resource)`

`LogExporter::set_resource` (`logs/export.rs:153`) and
`SpanExporter::set_resource` (`trace/export.rs:74`) are **defaulted to a no-op**
on the trait. `src/telemetry/otlp.rs` interposes four wrappers —
`ResilientHttpLogExporter`, `ResilientGrpcLogExporter`, `ResilientHttpSpanExporter`,
`ResilientGrpcSpanExporter` — each of which implements `export()` **and nothing
else**. Each therefore inherits the no-op `set_resource`, absorbs the Resource,
and never forwards it to the inner `opentelemetry_otlp` exporter. The inner
exporter serializes with its default (empty) Resource.

`OtlpSink::build_resource()` is correct and always has been. Its output is
simply thrown away one hop before the socket.

The metrics pipeline is **not** affected: `MetricExporter` is passed to
`PeriodicReader` unwrapped, which is why the collector's
`transform/metric_dimensions` rule (which reads `resource.attributes[...]`) was
written against a resource that actually exists.

This also explains why traces reaching Tempo carry no service identity — the
same defect, same commit, different signal.

### Root cause B — `TelemetryEvent.workspace` is dead

The dashboard's *first* choice for `repo` is the per-record log attribute
`workspace` (`normalize_otlp_log`: `attrs.get("workspace") or
data.get("workspace") or resource.get("needle.workspace")`).
`OtlpSink::emit_log` does set that attribute — but only
`if let Some(ref workspace) = event.workspace`, and **both** emit paths hardcode
the field to `None`:

- `Telemetry::emit` — `src/telemetry/mod.rs:3323`
- `Telemetry::emit_sync` — `src/telemetry/mod.rs:3488`

The field is declared in `TelemetryEvent` (`mod.rs:71`), documented in plan.md's
telemetry module spec, filterable via the query layer (`mod.rs:4023`), and
serialized into the file sink — and is never once populated. Fixing Root Cause A
alone leaves `repo` resolving through the resource fallback only.

### Root cause C — process-scoped identity cannot describe a roaming worker

`worker_telemetry_identity()` (`src/cli/mod.rs:849`) resolves identity **once, at
boot, from config**: `agent = config.agent.default`,
`workspace = config.workspace.default`, `model` = whatever that adapter declares.
Those become **Resource** attributes, and an OTel Resource is immutable for the
lifetime of the provider.

A NEEDLE worker is not process-scoped in any of those dimensions. `needle run -w`
says so in its own help text: *"NOT an exclusive scope. The Explore strand still
auto-discovers every directory containing `.beads/` under
`strands.explore.workspace_root`, so the worker can claim beads in other repos."*
A worker launched `-w /home/coding/aide-de-camp` will claim and dispatch in
`commitgraph`, `SEAM`, `tradegraph` — while its `needle.workspace` Resource
attribute says `aide-de-camp` forever. Model is likewise per-dispatch whenever
`agent.routing` is configured (it is, in this repo's `.needle.yaml`).

So even a fully working Resource pipeline would make the dashboard's **Repo**
field confidently wrong rather than blank — a worse failure than `Unknown`.

### Why the previous fix did not land

`needle-501aa991` ("Populate worker identity metadata on the OTLP event sink")
was closed by commit `519468a` at 2026-08-16 03:12 UTC. It correctly introduced
`TelemetryIdentity`, wired `Telemetry::from_config_with_identity` into
`run_worker`, and reduced the workspace to its basename. It did not — and could
not — make the values appear, because it never crossed the exporter-wrapper hop,
and its tests assert on `build_resource()`'s return value rather than on what an
exporter actually receives. The fleet binary (built 02:38 UTC) predates the
commit besides, so nothing shipped either.

**A test that asserts on `Resource` instead of on exported records cannot
observe this class of bug.** That is the reason it was closed as done while the
dashboard stayed blank.

## Decision

**Split worker identity across two OTLP layers, according to what is actually
invariant, and make the transport provably carry both.**

### 1. Forward `set_resource` through every exporter wrapper

Implement `set_resource` on all four resilient wrappers, delegating to the inner
exporter. The wrappers hold `inner: Arc<...>`; the SDK calls `set_resource` at
provider-build time while the `Arc` is still unique, so `Arc::get_mut` is the
delegation mechanism. A wrapper that cannot obtain the unique reference must log
at WARN rather than fail silently — a silently resource-less exporter is exactly
the failure being fixed.

**Rule adopted going forward:** any wrapper interposed on an OTel SDK exporter
trait must implement *every* method of that trait explicitly, including ones
with defaulted no-op bodies. Defaulted trait methods are the trap; `export()`
alone is never a complete implementation.

### 2. Identity that changes belongs on the record, not the Resource

| Attribute | Layer | Rationale |
|---|---|---|
| `service.name`, `service.version`, `service.instance.id`, `service.namespace` | Resource | Fixed for the process |
| `host.name`, `process.pid`, `needle.session_id` | Resource | Fixed for the process |
| `deployment.cluster`, `needle.worker.pool` (config `resource_attributes`) | Resource | Fixed for the host |
| `needle.agent`, `needle.model` | Resource **and** record | Resource carries the configured default; the record carries the adapter/model actually dispatched, which routing can change per bead |
| `workspace` (repo basename) | **Record** | Changes every time the worker roams; a Resource value is structurally incapable of being correct |

### 3. Populate `TelemetryEvent.workspace` at emit time

`Telemetry` gains a current-workspace cell that the worker updates when a claim
binds it to a workspace. `emit`/`emit_sync` read that cell instead of writing
`None`. `Bead.workspace` (`src/claim/mod.rs:609`) is already the authoritative
source and is known before dispatch, so no new plumbing crosses a module
boundary. Basename-only reduction stays where `519468a` put it, in
`workspace_label`, so full filesystem paths still never reach the browser.

### 4. Assert on exported records, not on builder output

Every telemetry-attribute test must run through the real
`build_http_providers` / `build_grpc_providers` path with a capturing exporter
substituted at the transport seam, and assert on the Resource and attributes the
exporter is handed. `519468a`'s `test_exported_log_record_contains_service_version`
is the right shape and the wrong depth — it swaps in a bare
`SdkLoggerProvider`, bypassing the wrapper that is the actual defect.

### 5. Verification is a wire capture, not a green test

The exit criterion for this work is a raw OTLP payload captured from a real
`needle` binary containing the expected resource attributes, plus a live
dashboard snapshot with no `null` in `repo`, `harness`, `model`, `semver`, or
`worker_pool`. The isolated-capture harness used to produce Evidence 2 above is
cheap, safe against the live fleet, and should be checked in as a script.

## Consequences

**Positive**
- The dashboard's worker card becomes accurate rather than decorative, and
  `Repo` tracks a roaming worker instead of freezing at its launch workspace.
- Traces to Tempo regain service identity — currently every NEEDLE span lands
  with an empty Resource, which makes them near-unusable for correlation.
- VictoriaLogs' configured stream fields (`service.name`, `deployment.cluster`,
  `service.instance.id`) start receiving two of three from NEEDLE rather than
  relying entirely on collector upserts.
- `needle.worker.pool` and every other operator-supplied
  `telemetry.otlp.resource_attributes` entry begin working for the first time;
  they are currently silently discarded.

**Negative / accepted**
- A per-record `workspace` attribute is one more attribute on every log record.
  Cardinality is bounded by the repo count under `workspace_root` (tens), and it
  is a basename, not a path.
- Adding `needle.agent`/`needle.model` at both layers means two sources for one
  concept. The record value wins; the Resource value documents the configured
  default. This is stated in the semantic mapping so consumers do not guess.
- The dashboard server's `normalize_otlp_log` fallback chain
  (record → body → resource) already implements exactly this precedence, so no
  coordinated deployment is required — NEEDLE can ship first.

**Neutral**
- No change to the `Sink` trait, the file sink, the JSONL format, or the browser
  contract's shape. `schema_version` on the dashboard contract stays where it is;
  fields that were always specified simply stop being `null`.

## Alternatives Considered

### Put the workspace on the Resource and restart the worker on roam

Rejected. It would make `Repo` correct by making the worker disposable, throwing
away the session, its trace context, and its warm state on every hop between
repos. The roaming design (ADR-004) exists precisely so a worker can drain
several repos in one session.

### Have the collector derive `repo` from `service.instance.id`

Rejected. The worker id is `{agent}-{identifier}` — `claude-code-glm-4.7-armor-1`
— where the identifier is a NATO/operator label that only *usually* resembles a
repo name. `luna-commitgraph` and `seam-2` parse plausibly; `alpha`, `rota-1`,
and `otelprobe` do not. Inferring identity from a name string in the collector
would produce confidently wrong values and move the logic away from the only
component that actually knows the answer.

### Drop the resilient wrappers and use the OTLP exporters directly

Rejected. The wrappers implement the drop-detection and consecutive-failure
accounting that feeds `telemetry.otlp.dropped` — real operational signal. The
defect is a missing trait method, not the wrapper pattern.

### Fix only the exporter wrappers and ship

Rejected as insufficient. It would populate `harness`, `model`, `semver`, and
`worker_pool`, and would populate `Repo` with the boot-time workspace — which,
for a roaming fleet, is wrong for most workers most of the time. Root Cause C
must be addressed in the same phase or the dashboard trades a blank field for a
misleading one.
