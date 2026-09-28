# Attempt archive drain

Attempt archives are optional and **off by default**: `attempt_archive.enabled: false` means NEEDLE creates no archive spool and does not contact an upload sink. When enabled, NEEDLE writes bundles locally; an operator may install an external drain such as the rclone example below, but running ARMOR or any other sink is never required.

## What is captured and why

One archive is produced for each resolved dispatch attempt. Depending on the
`attempt_archive.include` settings, it contains:

- the NEEDLE trace files (`stdout.txt`, `stderr.txt`, `trace.jsonl`, and
  `metadata.json`), so an outcome can be reconstructed;
- the agent harness transcript, when the session file can be located; and
- the exact prompt bytes dispatched to the agent.

The archive is keyed by bead and attempt rather than by the worker's latest
state. That preserves the evidence for retries, failures, and later
resolution, including attempts that did not produce a code change. The bundle
is compressed as `tar.zst` by default; `compression: none` produces a `tar`
bundle.

NEEDLE is local-only at this boundary. It does not upload, retry an object
store request, or hold a sink credential. An external drain owns those
responsibilities and can be replaced without changing NEEDLE.

## Configuration reference

The archive section is global and is not workspace-overridable:

```yaml
attempt_archive:
  enabled: false                 # default off
  spool_dir: ~/.needle/spool     # local spool only; `~` is expanded
  include:
    trace: true
    harness_transcript: true
    prompt: true
  compression: zstd              # zstd (default) or none
  prune_local_after_spool: true
  doctor_warn_after_hours: 6     # stale spool/drain warning threshold
```

With `enabled: false`, every other key is inert. With `enabled: true`, the
producer writes only to `spool_dir`; it still does not upload anything. The
spool can therefore be used as a local archive without installing a drain.
`prune_local_after_spool` controls retention of the source trace after a
complete local spool pair has been accepted; it does not delete a spool pair.
`doctor_warn_after_hours` controls when `needle doctor` warns about the oldest
queued sidecar or the age of the last drain completion.

## A6 spool contract (verbatim)

> The spool is deliberately the only sink-facing contract. A complete bundle
> is published first, and its JSON sidecar is published last; a drain must
> ignore bundles that do not have a sidecar.

The producer creates this layout:

```text
<spool>/<host>/<workspace-slug>/<bead-id>/<attempt-id>.tar.zst
<spool>/<host>/<workspace-slug>/<bead-id>/<attempt-id>.json
```

For `compression: none`, the bundle suffix is `.tar`. The sidecar has the
same stem and contains the schema version, relative `bundle_path`,
`bundle_sha256`, `bundle_bytes`, `host`, `workspace_slug`, `bead_id`, and
`attempt_id`, along with attempt metadata and the list of included files. A
drain must verify the byte count and SHA-256 recorded in the sidecar before
uploading the pair.

The bundle is first written as `<attempt-id>.tar.zst.partial` (or `.tar.partial`)
and atomically renamed. The sidecar is also atomically published. A drain
must skip `.partial` files and leave a bundle without its sidecar untouched.
`last-drain.json` is drain-owned diagnostic state and has this shape:

```json
{"finished_at":"...","uploaded":0,"failed":0,"bytes":0}
```

## Reference rclone drain

The checked-in reference implementation is
`contrib/attempt-archive/drain.sh`. It requires `rclone`, `jq`, and
`sha256sum` and accepts the spool, rclone config path, remote name, and bucket
as explicit arguments:

```bash
contrib/attempt-archive/drain.sh \
  --spool-dir ~/.needle/spool \
  --rclone-config ~/.config/rclone/rclone.conf \
  --remote armor \
  --bucket iad-ci
```

The rclone config must not be group- or world-readable. The drain rejects an
explicit non-TLS endpoint. `--allow-insecure` is only for a trusted local
`rclone serve s3` test endpoint; do not use it for a real sink. A correctly
sized remote object is treated as an idempotent success, so a retry verifies
the remote bundle and sidecar and then removes the local pair. A failed
upload or verification leaves the local pair for the next timer run and
returns non-zero.

The user units run this drain every five minutes with jitter:

```bash
install -Dm755 contrib/attempt-archive/drain.sh \
  ~/.local/share/needle/attempt-archive/drain.sh
install -Dm644 contrib/attempt-archive/needle-attempt-archive-drain.service \
  ~/.config/systemd/user/needle-attempt-archive-drain.service
install -Dm644 contrib/attempt-archive/needle-attempt-archive-drain.timer \
  ~/.config/systemd/user/needle-attempt-archive-drain.timer
systemctl --user daemon-reload
systemctl --user enable --now needle-attempt-archive-drain.timer
```

The service's `RCLONE_CONFIG` environment entry names a mode-600 file. The
remote and bucket in the unit are example operator settings; change them in
the unit if a different sink is selected. Credentials are never read from
NEEDLE configuration or a worker environment file.

## Object-key layout

Each local pair becomes two objects under the same prefix:

```text
<remote>:<bucket>/transcripts/<host>/<workspace-slug>/<bead-id>/<attempt-id>.tar.zst
<remote>:<bucket>/transcripts/<host>/<workspace-slug>/<bead-id>/<attempt-id>.json
```

When `compression: none`, the first object ends in `.tar`. The `transcripts/`
prefix is deliberately stable so a sink can grant access to only this archive
namespace and a later indexer can enumerate it without knowing NEEDLE's local
paths.

## Credential handling is by reference

Configuration names a credential source; it never contains the credential
value. The source may be an OpenBao path or a file path such as
`~/.config/rclone/rclone.conf`. Keep the file mode 600 and make the systemd
unit reference its path only. For example, an operator can materialize a
file from OpenBao without putting its value in shell history, documentation,
NEEDLE configuration, or a worker environment file:

```bash
umask 077
mkdir -p ~/.config/rclone
bao-as ... kv get -field=... ... > ~/.config/rclone/rclone.conf
chmod 600 ~/.config/rclone/rclone.conf
```

The `...` placeholders above are references to the operator's OpenBao
instance, path, and field; they are not credential values. The drain reads
the named file directly and never prints its contents.

## Using ARMOR as the sink

ARMOR is one reference sink, not a NEEDLE requirement. It exposes an
S3-compatible TLS endpoint; the fleet transcript receiver example uses
`https://transcripts-iad-ci-ts.ardenone.com:8444` with bucket `iad-ci` and
the `transcripts/` prefix. In the drain, those values are represented by the
rclone remote and `--bucket iad-ci`; the object keys remain the layout above.

ARMOR named credentials are prefix-scoped read+write today. The narrower
PUT-only policy is pending ADR-012 Decision 2. Encryption at rest is ARMOR's
responsibility, not NEEDLE's, so verify the selected ARMOR policy before
enabling a production drain.

Any S3-compatible sink can replace ARMOR. An operator may also keep the
archive local and omit the drain entirely.

## Restoring and reindexing

The sidecar is the index record for its bundle. To restore a subtree, copy the
objects from the sink into a staging directory and verify every downloaded
bundle against its sidecar:

```bash
rclone copy armor:iad-ci/transcripts/<host>/<workspace-slug>/<bead-id> \
  ./restored/<bead-id>
sha256sum ./restored/<bead-id>/<attempt-id>.tar.zst
```

The downloaded bundle is under the attempt directory. Use the sidecar's
`bundle_sha256` as the expected value.

For a broad reindex, enumerate `transcripts/` recursively with
`rclone lsjson`, group objects by the five path components after the prefix,
and use the JSON sidecars to recover attempt metadata and the expected bundle
hash/size. Do not infer an attempt from a missing sidecar, and do not delete
objects while reindexing. After verification, extract a bundle with
`tar --zstd -xf` (or the equivalent zstd tool) and use `trace/`, `harness/`,
and `prompt.md` according to the `contents` list in the sidecar.

## Sensitivity and access control

Bundles contain agent transcripts, traces, and exact prompts. An agent may
have pasted a secret into a transcript or prompt even when the normal
sanitizer removed known patterns. Treat every bundle as sensitive: the sink
must be encrypted at rest and access-controlled, and restored copies need the
same protection. Limit credentials to the `transcripts/` prefix and rotate
them through the operator's secret manager.
