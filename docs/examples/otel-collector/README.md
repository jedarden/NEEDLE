# OpenTelemetry Collector Example

This example demonstrates how to receive NEEDLE telemetry via OTLP and visualize it with open-source observability tools.

## Quick Start

1. **Start the observability stack:**

```bash
cd docs/examples/otel-collector
docker-compose up -d
```

This starts:
- **OpenTelemetry Collector** (ports 4317/gRPC, 4318/HTTP)
- **Jaeger** (UI: http://localhost:16686) — trace visualization
- **Prometheus** (UI: http://localhost:9090) — metrics storage
- **Loki** (port 3100) — log aggregation
- **Grafana** (UI: http://localhost:3000, admin/admin) — unified dashboards

2. **Configure NEEDLE to export OTLP:**

In your workspace `.needle.yaml`:

```yaml
telemetry:
  otlp_sink:
    enabled: true
    endpoint: "http://localhost:4317"
    protocol: "grpc"
```

3. **Start NEEDLE:**

```bash
needle work --workspace /path/to/workspace
```

4. **View the data:**

| What | URL |
|------|-----|
| Traces | http://localhost:16686 (Jaeger UI) |
| Metrics | http://localhost:9090 (Prometheus UI) |
| Logs | http://localhost:3000 (Grafana with Loki data source) |
| Dashboard | http://localhost:3000 (Grafana) |

## Verifying Data Flow

1. **Check collector health:**
```bash
curl http://localhost:8888/metrics
```

2. **Query NEEDLE metrics in Prometheus:**
```promql
rate(needle_beads_completed_total[5m])
histogram_quantile(0.95, needle_beads_duration_ms_bucket)
```

3. **Search traces in Jaeger:**
- Service: `needle`
- Operation: any (or filter by `bead.lifecycle`, `agent.dispatch`, etc.)

4. **View logs in Grafana:**
- Add Loki data source: http://loki:3100
- Query: `{job="needle"}`

## Stopping

```bash
docker-compose down
```

## Customization

- **Change endpoints:** Edit `docker-compose.yml` port mappings
- **Add more backends:** Edit `otel-collector-config.yaml` to add exporters (e.g., Tempo, Honeycomb, Datadog)
- **Adjust resource limits:** Add `mem_limit` and `cpus` to services in `docker-compose.yml`

## Troubleshooting

- **No traces in Jaeger:** Check that NEEDLE is using the correct endpoint (4317 for gRPC, 4318 for HTTP)
- **Collector logs:** `docker-compose logs -f otel-collector`
- **Port conflicts:** Ensure ports 16686, 9090, 3000, 4317, 4318 are available
