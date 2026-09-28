# Lab fleet test data

## `lab-adapters/` — mirror of lab's real adapter directory

Lab keeps its worker adapter YAMLs machine-local at
`lab:~/.config/needle/adapters` (never in this repo — see
`docs/adapter-cargo-environment.md`), which means no test running anywhere
else can see them. That gap hid real drift for weeks: the cargo-environment
checker's `--self-test` passed on synthetic fixtures while its parser
misread the `systemd-run ... bash -c '...'` nested-quote idiom
(`CARGO_BUILD_JOBS='\''2'\''`) used by lab's real GLM adapters, false-positived
on them, and blocked every `apply-lab-fleet.sh` run at its policy gate
(needle-01dea2ea, 2026-09-27).

This directory is a verbatim copy of the `*.yaml` set (the checker only reads
`*.yaml`; the prompt-fragment `.md` files are not mirrored) from
2026-09-28T08:19Z, i.e. **after** `RUSTFLAGS` was removed from
`claude-code-glm-4.7.yaml` and `claude-code-glm-5.3-flash.yaml` per the
fleet-wide decision in `docs/adapter-cargo-environment.md`. One real file is
deliberately not mirrored: lab's `test-echo.yaml` is a synthetic E2E adapter
whose invoke_template closes beads with the retired `br` CLI, which this
repo's tree policy forbids; the live-directory check below still covers it
whenever the test runs on lab. The files carry
no credentials — every auth token in the real set is the `proxy-handles-auth`
placeholder; re-verify that before each refresh (see below).

`fleet/lab/test.sh` runs `scripts/check-adapter-cargo-environment.sh` over
this mirror on every host, and over the live `$HOME/.config/needle/adapters`
when one exists, so both checker regressions against lab's real shapes and
drift in the live directory itself fail the test instead of the next deploy.

### Refreshing the mirror

When lab's real adapter set changes shape (a new adapter, a new wrapper
idiom, a policy-block edit), refresh the mirror so the fixture keeps
exercising what lab actually runs:

```bash
mkdir -p fleet/lab/testdata/lab-adapters
scp 'lab:~/.config/needle/adapters/*.yaml' fleet/lab/testdata/lab-adapters/
# then re-check for credential material before committing:
grep -rliE 'sk-[a-zA-Z0-9]{10,}|ghp_[a-zA-Z0-9]|xox[bap]-|AIza[a-zA-Z0-9_-]{20}' \
  fleet/lab/testdata/lab-adapters/   # must print nothing
```

Committing a mirror that differs from lab's live set is fine only while the
difference is the thing under test (e.g. verifying a policy fix before
applying it to lab) — note it in the commit message, then follow up with the
real refresh.
