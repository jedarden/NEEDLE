# ZCode headless adapter

This plugin runs the CLI bundled in the official ZCode desktop package as a
one-shot NEEDLE development agent. ZCode 3.10.2 bundles CLI 0.16.5, whose
headless surface accepts a prompt, workspace, permission mode, turn limit, and
machine-readable JSON output.

## Prerequisites

- Install the official ZCode desktop package.
- Connect the Z.ai Coding Plan through ZCode's supported login flow.
- Select `GLM-5.3-Flash` in ZCode before starting workers. CLI 0.16.5 does not
  expose a `--model` flag, so the adapter intentionally uses the model selected
  in ZCode settings and reports the requested model as `zcode-selected`.
- Keep the first canary at one ZCode worker until concurrent session and quota
  behavior has been measured.

The adapter never accepts an API key or copies ZCode's credential store. If a
dedicated settings file is required, set `NEEDLE_ZCODE_SETTINGS` to its path;
only the path is passed to `zcode --settings`.

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
  --mode yolo --max-turns 100 --json --no-color
```

The prompt is read from NEEDLE's temporary prompt file and passed as one argv
element without shell evaluation. ZCode 0.16.5 does not offer prompt input on
stdin. Consequently, prompt text is visible to same-user process inspection
while a task runs; do not put credentials in bead text. The wrapper replaces
itself with the ZCode process so NEEDLE observes the real exit status and its
timeout or cancellation signal reaches ZCode's process group.

`NEEDLE_ZCODE_MODE`, `NEEDLE_ZCODE_MAX_TURNS`, `NEEDLE_ZCODE_SETTINGS`, and
`NEEDLE_ZCODE_CLI` can override their corresponding defaults. Credentials are
deliberately not configurable through this wrapper.

The bundled `zcode app-server` stdio protocol is not used in this first adapter
because Z.ai has not documented its request, cancellation, or compatibility
contract. It can be evaluated later as a way to avoid prompt text in argv and
reuse warm sessions.
