# Production OTLP Configuration - ex44

**Date:** 2026-08-15  
**Bead:** needle-a78d47ae  
**Host:** ex44 (Hetzner)  
**Configuration File:** `/home/coding/.config/needle/config.yaml`

## Change Summary

Enabled OTLP telemetry export in the production NEEDLE worker configuration on ex44.

### Configuration Change

**Before:**
```yaml
telemetry:
  otlp_sink:
    enabled: false
    endpoint: http://needle-otel-ex44-apexalgo-iad-ts.ardenone.com:4318
```

**After:**
```yaml
telemetry:
  otlp_sink:
    enabled: true
    endpoint: http://needle-otel-ex44-apexalgo-iad-ts.ardenone.com:4318
    protocol: http
    tls:
      insecure: true
      ca_file: ""
    headers:
      - "Authorization: env:NEEDLE_OTLP_AUTHORIZATION"
```

## OTLP Sink Configuration

The production OTLP sink is configured with the following settings:

| Setting | Value | Description |
|---------|-------|-------------|
| `enabled` | `true` | OTLP export is now active |
| `endpoint` | `http://needle-otel-ex44-apexalgo-iad-ts.ardenone.com:4318` | OTLP HTTP endpoint (Tailscale hostname) |
| `protocol` | `http` | HTTP/protobuf transport |
| `timeout_secs` | `10` | Request timeout in seconds |
| `compression` | `gzip` | Payload compression |
| `tls.insecure` | `true` | No certificate verification; traffic remains inside the Tailscale network |
| `tls.ca_file` | `""` | Use no custom CA file |
| `headers` | `Authorization: env:NEEDLE_OTLP_AUTHORIZATION` | Read the complete authorization value from the environment |
| `metrics_interval_secs` | `10` | Metrics export interval |
| `service_namespace` | `needle-fleet` | Service namespace for resource attributes |
| `max_queue_size` | `2048` | Maximum queue size for batching |

### Resource Attributes

- `deployment.cluster=ex44` - Identifies the bare-metal cluster
- `needle.worker.pool=bare-metal` - Identifies the worker pool type

## Verification

Configuration syntax has been verified as valid YAML:

```bash
python3 -c "import yaml; yaml.safe_load(open('/home/coding/.config/needle/config.yaml'))"
# ✓ YAML syntax is valid
```

## Backup

A backup of the configuration file was created before changes:
```
/home/coding/.config/needle/config.yaml.bak-before-otlp-enable-20260815-130000
```

## Related Changes

- **Child bead:** needle-3d249a29 (local test worker boots successfully with OTLP)
- **Fix applied:** bf-3s2b0 (worker boots without crash from previous fix)

## Next Steps

The production worker configuration is now ready for launch with OTLP telemetry enabled. The next restart of the NEEDLE supervisor/workers will begin exporting telemetry to the OTLP collector.

## Notes

- The OTLP collector endpoint is hosted on `apexalgo-iad` cluster
- Traffic flows over Tailscale VPN for security
- Authorization is via a Bearer token supplied by the `NEEDLE_OTLP_AUTHORIZATION` environment variable; no secret is stored in this document or config YAML
- All production workers will now emit traces, metrics, and logs to the centralized collector
