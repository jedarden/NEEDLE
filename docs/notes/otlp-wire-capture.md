# Isolated OTLP wire capture

Use `scripts/otlp-wire-capture.sh` when an OTLP problem must be checked at the
socket boundary—for example, when NEEDLE and the collector are healthy but
resource attributes are missing in the backend. It starts a receiver on
`127.0.0.1` only, captures the raw HTTP/protobuf POST bodies, and returns an
empty protobuf response to the worker.

Run it from this repository:

```bash
NEEDLE_BIN=/path/to/needle scripts/otlp-wire-capture.sh
```

The source config defaults to `$HOME/.config/needle/config.yaml`. Override it
with `NEEDLE_CAPTURE_CONFIG` when the real config lives elsewhere. The script
creates a retained directory below `${TMPDIR:-/tmp}` containing:

- `otlp-payloads.bin`: exact POST bodies, appended in receive order;
- `otlp-payloads.strings`: printable strings extracted from that binary;
- `needle.log`: the probe worker's output.

The copied config is sanitized before it is written to scratch. OTLP is forced
to local HTTP with no compression, `headers: []`, and no Authorization header;
the script then scans the generated file and refuses to run if any
case-insensitive `Authorization:` remains. The probe runs with a scrubbed
environment, `HOME` and `XDG_CONFIG_HOME` under scratch, `workspace.home` under
scratch, and both `workspace.default` and `strands.explore.workspaces` pinned
to one newly initialized throwaway Git repository. It therefore does not
touch `~/.needle`, `~/.config/needle`, or a real workspace.

Read the wire contents with:

```bash
strings /tmp/needle-otlp-wire.XXXXXX/otlp-payloads.bin
```

Look for resource keys such as `service.name`, `service.version`,
`service.instance.id`, and the `needle.*` attributes. The worker normally exits
nonzero when the empty probe repository has no bead backend; that is expected.
The harness succeeds only when at least one OTLP body was captured.
