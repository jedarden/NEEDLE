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

## Automated pre-deployment check

Run `scripts/check-adapter-cargo-environment.sh` against each host-local
adapter directory before applying a fleet. It checks every declared Cargo
policy assignment, reports all violations in the directory, and fails the
deployment gate if any value is wrong, a partial Cargo block is present, or
`RUSTFLAGS` is set. An adapter with no Cargo block is allowed for adapters that
do not use this build policy.

The codinghome and lab apply scripts invoke the same checker before changing
deployed files. To check a counterpart manually:

```sh
scripts/check-adapter-cargo-environment.sh \
  --label codinghome --adapters-dir "$HOME/.config/needle/adapters"
```

Run `scripts/check-adapter-cargo-environment.sh --self-test` to exercise the
valid, no-policy, partial, wrong-value, and forbidden-`RUSTFLAGS` cases.

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

After a day of fleet traffic, verify Cargo fingerprints newer than the marker
`/home/coding/.needle/needle-2020b478-rustflags-marker` and record the output
on bead `needle-2020b478`. The cargo wrapper now gives each repository its own
target under `/build/<repo>`, so search those targets rather than the retired
shared target path:

```sh
find /build -type f -path '*/debug/.fingerprint/*/lib-tokio.json' -newer /home/coding/.needle/needle-2020b478-rustflags-marker -print0 | xargs -0 -r jq -c .rustflags | sort | uniq -c
```

Workers load the adapter table at process start. Restart all active
`needle-worker@*` user services on codinghome after the adapter change, then
refresh the marker before running the check above. Fingerprint directories
written under the old flags keep their own hashes forever; an unrefreshed
marker would keep reporting them.

## 2026-09-25 fleet restart snapshot

At 2026-09-26T03:46:19Z, all 32 active codinghome worker services had
`ActiveEnterTimestamp` values later than the 2026-09-25T01:26:53Z adapter
cutover, and systemd had no restart jobs pending. Two retired units remained
masked and inactive. The 33-adapter policy check passed. A live environment
scan of 156 processes in the 32 worker service cgroups found no `RUSTFLAGS`
assignments, so no active worker was observed using the retired
`-C codegen-units=1` flag.

The documented legacy target `/data/build/target-workers/debug/.fingerprint`
was absent during this audit. The equivalent query over the current
per-repository targets under `/build` exited 0 and produced no output: no
`lib-tokio.json` fingerprint had been written after the refreshed marker yet.
This immediate post-restart snapshot identifies no worker using the retired
flag, but it contains no post-marker Cargo fingerprint; repeat the query after
workers have built with the refreshed adapter table to collect fingerprint
evidence from fleet traffic.
