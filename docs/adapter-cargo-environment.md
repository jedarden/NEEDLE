# Cargo environment for fleet adapters

## Shared `RUSTFLAGS` value

The fleet adapter contract leaves `RUSTFLAGS` unset, so Cargo and rustc use
their defaults. Do not add `-C codegen-units=1` to an individual adapter. When
copying the GLM cargo environment block to another adapter, copy
`CARGO_BUILD_JOBS=2`, `CARGO_INCREMENTAL=0`, and `RUST_TEST_THREADS=2`, but
leave `RUSTFLAGS` out.

The codinghome adapter files changed for this decision are:

- `~/.config/needle/adapters/claude-code-glm-4.7.yaml`
- `~/.config/needle/adapters/claude-code-glm-5.yaml`
- `~/.config/needle/adapters/claude-code-glm-5.3.yaml`
- `~/.config/needle/adapters/claude-code-glm-5.3-flash.yaml`
- `~/.config/needle/adapters/claude-code-glm-5-turbo.yaml`
- `~/.config/needle/adapters/glm.yaml`

The active codinghome codex, claude-print, opencode, and omp adapters already
left `RUSTFLAGS` unset. The tracked ex44 codex templates do too. The lab fleet
uses machine-local adapter YAMLs under `~/.config/needle/adapters`; keep those
counterparts on the same unset value. `fleet/lab/workers.tsv` tracks worker
membership, not those host-local adapter files.

## Measurement used for the decision

On 2026-09-24, `cargo build --all-targets` was measured in clean source
extractions of bead-rs and the TRACE analytics crate. Each run used a distinct
target directory and ran under `systemd-run --user --scope -p MemoryMax=6G -p
CPUQuota=200%`, with `CARGO_BUILD_JOBS=2`, `CARGO_INCREMENTAL=0`, and
`RUST_TEST_THREADS=2`. Peak RSS and wall time came from GNU Time 1.10 at
`/run/current-system/sw/bin/time -v` (the host does not provide `/usr/bin/time`).

| Build | `RUSTFLAGS` | Peak RSS, runs 1 / 2 / 3 (GiB) | Median peak RSS (GiB) | Wall time, runs 1 / 2 / 3 (s) | Median wall time (s) |
|---|---|---:|---:|---:|---:|
| bead-rs | unset | 0.825 / 0.830 / 0.831 | 0.830 | 68.25 / 60.12 / 64.74 | 64.74 |
| bead-rs | `-C codegen-units=1` | 1.386 / 1.388 / 1.387 | 1.387 | 55.50 / 65.87 / 57.96 | 57.96 |
| TRACE analytics | unset | 3.548 / 3.543 / 3.546 | 3.546 | 705.21 / 703.57 / 736.56 | 705.21 |
| TRACE analytics | `-C codegen-units=1` | 3.101 / 3.399 / 3.409 | 3.399 | 766.73 / 806.81 / 728.30 | 766.73 |

The unflagged maximum was 0.831 GiB for bead-rs and 3.548 GiB for TRACE
analytics. All 12 builds exited successfully. Both unflagged maxima are below
the 4.5 GiB decision threshold, so the adapters use the default rustflags.
For TRACE analytics, the unset median was 61.52 seconds faster than
`-C codegen-units=1`; the bead-rs result went the other way by 6.78 seconds.
The flag increased bead-rs median peak RSS from 0.830 GiB to 1.387 GiB, while
neither build needed it to stay under the threshold.

After a day of fleet traffic, verify the shared target fingerprints using the
marker `/home/coding/.needle/needle-2020b478-rustflags-marker` and record the
output on bead `needle-2020b478`:

```sh
find /data/build/target-workers/debug/.fingerprint -name 'lib-tokio.json' -newer /home/coding/.needle/needle-2020b478-rustflags-marker | xargs jq -c .rustflags | sort | uniq -c
```
