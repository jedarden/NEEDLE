# ZCode headless adapter

This plugin runs the CLI bundled in the official ZCode desktop package as a
one-shot NEEDLE development agent. ZCode 3.10.2 bundles CLI 0.16.5, whose
tested headless surface accepts a prompt, workspace, permission mode, and
streaming machine-readable output.

## Prerequisites

- Install the official ZCode desktop package.
- Configure either Z.ai Coding Plan login or an Anthropic-compatible provider
  in ZCode's default `~/.zcode/cli/config.json`.
- Select `GLM-5.3-Flash` before starting workers. CLI 0.16.5 does not expose a
  working `--model` flag, so the adapter uses the model selected in the config.
- Keep the first canary at one ZCode worker until concurrent session and quota
  behavior has been measured.

The adapter never accepts an API key or copies ZCode's credential store.

### Arden One Z.AI proxy

For the internal proxy, start from
[`zcode-zai-proxy.example.json`](zcode-zai-proxy.example.json), set
`network.caCertFile` to the current apexalgo-iad cluster CA PEM, and install it
as `~/.zcode/cli/config.json` with mode `0600`. The placeholder API key is
intentional: the proxy replaces it with its upstream credential.

Keep TLS validation enabled. If the proxy leaf was issued by an expired or
rotated CA, renew that leaf through GitOps and refresh the local CA file; do not
set `NODE_TLS_REJECT_UNAUTHORIZED=0`.

## Install

```bash
cd plugins/zcode-headless
./install.sh
```

ZCode's desktop installer does not always put its bundled CLI on `PATH`. Point
the installer at the official runtime when automatic discovery cannot find it:

```bash
./install.sh \
  --zcode-cli /opt/ZCode/resources/glm/zcode.cjs
```

The installer stores that path—not any credential—in
`~/.config/needle/zcode-cli-path` with mode `0600`.

Verify the adapter before dispatch:

```bash
needle-zcode-headless --preflight
needle test-agent zcode-headless
```

Run a single canary worker in an explicitly chosen workspace:

```bash
needle run --agent zcode-headless --workspace /path/to/repository --count 1
```

## Runtime contract

The wrapper invokes the bundled runtime as:

```text
zcode --prompt <prompt> --cwd <workspace> --surface terminal \
  --mode yolo --output-format stream-json --no-color
```

The prompt is read from NEEDLE's temporary prompt file and passed as one argv
element without shell evaluation. ZCode 0.16.5 does not offer prompt input on
stdin. Consequently, prompt text is visible to same-user process inspection
while a task runs; do not put credentials in bead text. Stream JSON keeps
NEEDLE's idle watchdog informed during model and tool activity. The wrapper
replaces itself with the ZCode process so NEEDLE observes the real exit status
and its timeout or cancellation signal reaches ZCode's process group.

Although 0.16.5's help lists `--settings` and `--max-turns`, its actual parser
rejects both. The adapter therefore uses the default config path and relies on
NEEDLE's idle and hard timeouts. `NEEDLE_ZCODE_MODE` and `NEEDLE_ZCODE_CLI` can
override their corresponding defaults. Credentials are deliberately not
configurable through this wrapper.

The bundled `zcode app-server` stdio protocol is not used in this first adapter
because Z.ai has not documented its request, cancellation, or compatibility
contract. It can be evaluated later as a way to avoid prompt text in argv and
reuse warm sessions.
