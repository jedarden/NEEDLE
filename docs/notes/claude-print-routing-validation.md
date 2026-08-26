# claude-print routing host validation

Bead `needle-4ddfbf70` tracks a live end-to-end check of NEEDLE's model-based
adapter routing. The canonical host test is
`tests/integration/test_claude_print_routing.sh`. It creates isolated Git and
bead-rs workspaces under `/home/coding/scratch`, uses the host's real adapter
definitions and agent credentials, and emits file-sink telemetry into its
temporary home.

The test is deliberately excluded from automated suites because it consumes
real agent capacity and, for the missing-binary scenario, briefly renames the
host's `claude-print` executable. A trap restores the executable on success,
failure, or interruption. Failed runs retain their isolated artifacts for
diagnosis.

## Host result: 2026-08-25 EDT

Tested with the development NEEDLE 0.5.0 binary, `claude-print` 0.2.0, and
Claude Code 2.1.246.

| Scenario | Result | Evidence |
| --- | --- | --- |
| Sonnet subscription route | **Blocked** | Bead `route-5bec8b94` emitted `agent.routing_decision` with model `claude-sonnet-4-6` and `chosen_adapter: claude-print`. The invocation probe recorded `claude-print`, model `claude-sonnet-4-6`, and output format `json`. The adapter then exited 2, so the bead did not complete. |
| GLM 4.7 negative control | **Pass** | Bead `route-1de861b2` completed through `claude-code-glm-4.7`. Its routing event recorded model `glm-4.7` and `chosen_adapter: claude-code-glm-4.7`; both normalized agent output and the bead trace parsed as JSONL. |
| Routing-decision telemetry | **Pass** | Real `agent.routing_decision` events were observed for both Sonnet and GLM dispatches, with the requested model, matched rule, and chosen adapter. |
| Missing `claude-print` | **Pass** | Bead `route-89a91309` routed to `claude-print`, emitted `agent.completed` with exit 127, remained open, and emitted no completion for a `claude-sonnet` fallback. The binary was restored immediately. |

The overall acceptance criterion is **not yet met**, because the real Sonnet
agent cannot start on this host. Running `claude-print` directly with its child
stderr enabled exposes the suppressed failure:

```text
error: unknown option '--timeout'
```

`claude-print` 0.2.0 adds a `--timeout` argument when it starts Claude Code,
but the installed Claude Code 2.1.246 rejects that option. NEEDLE correctly
routes to `claude-print`, records the invocation, and normalizes the adapter's
error result as JSON, but it cannot make the requested bead complete. Repairing
the external `claude-print`/Claude Code compatibility is outside this
repository; the bead must remain open until that host dependency is fixed and
the full test passes.

The live run also exposed two NEEDLE defects that the host test now exercises:

- `InputMethod::Stdin` adapters were not receiving the generated prompt on
  standard input.
- `split_after_failures: 0`, documented as disabling automatic split mode,
  instead selected split mode for every fresh bead.

Both have focused unit coverage in addition to the host harness.

## Running the test

Run every scenario from the NEEDLE repository:

```bash
NEEDLE_ROUTING_E2E_ACK=I_UNDERSTAND \
  tests/integration/test_claude_print_routing.sh
```

To validate individual controls while diagnosing a host dependency, select a
comma-separated subset:

```bash
NEEDLE_ROUTING_E2E_ACK=I_UNDERSTAND \
NEEDLE_ROUTING_E2E_SCENARIOS=glm47,missing \
  tests/integration/test_claude_print_routing.sh
```

`NEEDLE_BIN` may point to a specific build. Without an override, the harness
uses `~/.needle/bin/needle-stable`. The test refuses to run without the explicit
acknowledgement, refuses the rename scenario while another `claude-print`
process is active, records only the executable name/model/output format in its
invocation probe, and verifies restoration before reporting success.
