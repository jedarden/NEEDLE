# ADR-013: Pluggable Bead-CLI Backends — Three Upstreams, One Configurable Seam

**Status:** Proposed — 2026-08-11 (revised 2026-08-12; see Revision History)
**Deciders:** operator (jedarden), via Claude Code
**Tracking:** see Phase 16 in `docs/plan/plan.md`; beads filed under label `bead-cli-backend`

## Context

NEEDLE must drive **three** independently-evolving bead CLIs, not one canonical tool plus legacy debris:

| Priority | Backend | Binary | Upstream | Installed here |
|---|---|---|---|---|
| **primary** | **bead-rs** | `bead` | `git.ardenone.com/jedarden/bead-rs` | agent-sandbox `needle-pod`, v0.1.1 |
| **secondary** | **bead-forge** | `bf` | `git.ardenone.com/jedarden/bead-forge` | `~/.local/bin/bf`, v0.4.1 |
| **tertiary** | **beads_rust** | `br` | `github.com/dicklesworthstone/beads_rust` | `~/.cargo/bin/br`, v0.1.28 |
| *(open world)* | any other | — | third-party, future, forks | not installed |

The priority column is an operator decision recorded in draft 4; see §7. It sets
which backend the defaults favour and which descriptor is written first — it does
**not** reduce the others to second-class. All three stay first-class, and the
open-world row is a requirement, not a courtesy: NEEDLE must be able to drive a
bead CLI that did not exist when NEEDLE was compiled.

(The original Go `beads` — `bd` v0.49.6, `~/go/bin/bd`, the common ancestor of all
three — is the nearest concrete instance of that fourth row, and is why the three
dialects rhyme without matching.)

The immediate trigger is the `game-of-life` project in the **agent-sandbox** cluster, whose workspace is bead-rs-backed. Stock NEEDLE cannot drive it: a worker booted there fails at the first claim. But the investigation found the problem is broader than one missing backend, and that NEEDLE's *existing* handling of the two backends it nominally supports is already incorrect.

### The seam is right; what leaks past it is not

`trait BeadStore` (`src/bead_store/mod.rs:546`) already exists, is `Send + Sync`, and is what every consumer holds — `Arc<dyn BeadStore>` in `Worker` (`src/worker/mod.rs:306-308`), in `Claimer`, in every strand. Two impls already exist: `BrCliBeadStore` (`:701`) and `BfCliBeadStore` (`:1852`). Nothing in the worker or strand layer knows which CLI is underneath. **That part of the design is sound and does not change.**

Four things leak past it.

### 1. Binary resolution is hardcoded in five places, one of which bypasses the trait

| Site | Behavior |
|---|---|
| `src/bead_store/mod.rs:758-775` | `BrCliBeadStore::discover` — chain `bf` → `~/.local/bin/bf` → `br` → `~/.local/bin/br` |
| `src/bead_store/mod.rs:1127-1136` | a **second**, independent `resolve_bf()` used only by `run_bf_batch` / `run_bf_claim` |
| `src/bead_store/mod.rs:1893-1902` | `BfCliBeadStore::discover` — a third copy |
| `src/worker/mod.rs:732-742` | boot-time version handshake, `which::which("bf")` inline |
| `src/cli/mod.rs:3626-3632` | `needle doctor` preflight — `bf` else `br` else fail `"no bead store CLI found on PATH (checked bf, br)"` |
| `src/validation/predispatch.rs:128` | `run(workspace, "bf", &["show", …])` — **bypasses `BeadStore` altogether** |

The first row is the load-bearing defect. **`BrCliBeadStore::discover` resolves `bf` first** — so the store that speaks beads_rust dialect is bound, by default, to the bead-forge binary. See §5.

### 2. Three dialects, diverged from a common ancestor in three directions

Enumerated from each installed binary's own `--help` on 2026-08-11. This is the matrix any backend abstraction has to absorb:

| Operation | `br` 0.1.28 | `bf` 0.4.1 | `bead` 0.1.0 |
|---|---|---|---|
| atomic server-side claim | **absent** | `claim --model --harness --harness-version --assignee --json` | `claim --assignee --json --policy` (no velocity metadata) |
| transactional batch | **absent** | `batch --json '[…]'` | **absent** |
| create: description | `-d/--description` (**`--body` is a valid alias**) | `--description` only | `--description` only |
| create: labels | `-l/--labels` **comma-separated** | repeated `--label` | repeated `--label` |
| create: quiet ID | `--silent` | absent (bare ID on stdout) | absent (bare ID on stdout) |
| `dep add` | `<ISSUE> <DEPENDS_ON> -t blocks` — **2 positionals** | `<BLOCKER> --blocks <BLOCKED> -t` — **1 positional** | `<BLOCKED> <BLOCKER> --kind blocks` — **2 positionals** |
| `update --assignee` | present | **removed in 0.4.1** (`bf-1hmey`) | present, plus `--clear-assignee` |
| `sync --import-only` | bare | bare | requires `--input` + one of `--restore-into-empty`/`--merge` |
| ready frontier | `ready --json --limit` | `ready` | `list --ready` |
| `doctor --repair` | present | present | present |
| close reason | `-r/--reason` | `--reason` | `--reason` |
| machine-readable contract | `schema`; **`capabilities` + `robot-docs` as of 0.2.22** | `schema`, `robot-docs` | `capabilities --profile` |

Two structural facts fall out. **`bf` is the only backend with a transactional `batch`**, so it is the only one that can make mitosis atomic. **`bf` is the only backend that dropped `update --assignee`**, so it is the only one that *needs* `batch` for something as ordinary as claiming. Those two facts are the same fact, and together they set the bar for any backend abstraction: the backends do not differ only in flag spelling, they differ in which operations are single commands at all. A configuration format that captures only argv cannot express that difference — which is why the Decision below pairs argv templates with a per-operation *strategy*.

Note also that `dep add`'s positional order agrees between `br` and `bead` (blocked first, blocker second) and `bf` is the outlier. Getting this backwards inverts every dependency edge rather than failing loudly — the one place in this ADR where a wrong guess is worse than a crash.

### 3. The prompt hands the agent a hardcoded dialect

`src/prompt/mod.rs` embeds `bf update` (`:56`), `bf close` (`:64`), and a whole `bf create` / `bf show` / `bf dep add` / `bf label add` block in the mitosis-split instructions (`:294-325`), with a test asserting the literal string (`:1066-1067`). An agent in the bead-rs sandbox is told to run a binary that is not installed there.

### 4. Version-handshake logic is bead-forge-specific

`check_bead_forge_version` / `run_version_handshake` (`:295-419`) parse bead-forge version strings to detect bf 0.2.0's `--limit 0` bug. Against `br --version` or `bead --version` this degrades to a warning, but the `--limit 999999` workaround it exists to justify is applied unconditionally at `:1357` and `:2094`.

### 5. `BrCliBeadStore` is a *correct* beads_rust store bound to the wrong binary

This is the finding that reframes the rest. Checked against the real `br` 0.1.28 at `~/.cargo/bin/br`:

```
create --title T --body B --json --silent --labels a,b      (:1550-1563)
dep add <blocked> <blocker> --type blocks                   (:1579)
sync --import-only                                          (:1706)
list --json --limit 999999                                  (:1357)
doctor --repair                                             (:1675)
```

**Every one of these is valid beads_rust.** `--body` is a documented alias of `-d/--description`; `--silent` exists; `-l/--labels` is comma-separated; `dep add` takes two positionals with `-t` defaulting to `blocks`; `sync --import-only` is bare; `list` has `--json` and `--limit`. `BrCliBeadStore` is not stale, drifted, or wrong. It is a faithful beads_rust adapter.

The defect is that **`discover()` (`:758-775`) resolves `bf` before `br`**, so on this host — and on every fleet host, where `~/.local/bin/bf` is present — the beads_rust adapter sends beads_rust argv to the bead-forge binary, which rejects most of it. `bf 0.4.1 create` has no `--body`, no `--silent`, no `--labels`. `bf 0.4.1 dep add` takes one positional, not two.

Compounding it, bf-only calls were grafted *into* the beads_rust store: `claim` (`:1428-1437`), `release` (`:1479-1486`), and `split` (`:1485`, `:1508`) all route through `bf batch` via the second resolver at `:1127`, and `claim_auto` shells to `bf claim` (`:1194-1220`). beads_rust has neither subcommand. So `BrCliBeadStore` is now a chimera: beads_rust argv for create/dep/sync/list, bead-forge argv for claim/release/split, and a resolver that picks whichever binary it finds first regardless of which half is being invoked.

There is exactly one genuine argv bug, and it is in the *other* store: `BfCliBeadStore::add_dependency` (`:2288`) emits two positionals, which is correct for `br` and `bead` but wrong for `bf`.

**Consequence in production:** every worker path constructs `BrCliBeadStore` — `worker/mod.rs:1385`, `supervisor/mod.rs:214`, `strand/explore.rs:71`/`:450`, `strand/splice.rs:747`/`:839`/`:999`. `BfCliBeadStore` is built only in `cli/mod.rs:997` and `:3384`. So the fleet runs the chimera. `create_bead` fails with a clap parse error against `bf`, and through the trait's default `split_bead` (`:646-668`) that takes all of mitosis child creation with it.

### 6. Descriptors must pin the version they were verified against

The `br` binary installed on this host is **v0.1.28, built 2026-03-29**. Upstream beads_rust is at **v0.2.22 (2026-08-06)** — roughly twenty releases and four months ahead. Every dialect fact in §2 was originally derived from the stale local binary.

Downloading `br-0.2.22-linux_x86_64` from the GitHub release and re-probing confirms the dialect is stable across that gap on every point NEEDLE touches: `-d/--description` still carries `[alias: --body]`, `-l/--labels` is still comma-separated, `--silent` survives, `dep add <ISSUE> <DEPENDS_ON> -t` still takes two positionals, `update -s --assignee` is still present, and there is still no `claim` and no `batch`. That is luck, not design — and it is exactly the check a descriptor should force rather than leave to chance.

0.2.22 does add subcommands: `capabilities`, `robot-docs`, `capacity`, `coordination`, `gate`, `scheduler`, `vcs-status`. The first two matter here — **all three backends expose a capabilities-style contract surface**, so §4's negotiation is uniform rather than a bead-rs special case.

Two consequences for the design:

- A descriptor carries `verified_against` (binary version) and the date it was checked. A fleet host running v0.1.28 while upstream ships v0.2.22 is normal; a descriptor that does not say which one it was written for is a guess.
- **bead-rs is under active development in agent-sandbox**, so its surface is a *moving* target, not a stale one: the local `~/bead-rs` is nine commits past the `fa30574` this ADR cites, including `20853cf`, which fixed a defect that bricked every fresh clone. Its descriptor must be verified against the binary the sandbox actually runs, and re-verified when that moves. Unlike `br` and `bf`, bead-rs publishes **no GitHub releases** — it is Forgejo-only — so there is no release artifact to pin to and the descriptor must name a commit.

## Decision

**One descriptor-driven `CliBeadStore`, with backends defined as data — not one hardcoded impl per upstream.**

NEEDLE already solves this exact problem for AI agent harnesses and the bead layer should not invent a second answer. `AgentAdapter` (`src/dispatch/mod.rs`) is a serde struct; `load_adapters` (`:607-660`) merges `builtin_adapters()` (`:570-579`) with user YAML dropped in `~/.config/needle/adapters/`, user files overriding built-ins by name. Adding a new agent harness requires no recompile. The bead CLI gets the same shape.

### 1. `BeadBackend` descriptors, loaded like adapters

A `BeadBackend` is a serde struct describing one CLI: binary name, identity check, detection paths, per-operation argv templates, and a declared capability set. `builtin_bead_backends()` ships `beads_rust`, `bead-forge`, and `bead-rs` as **data, not code**; `load_bead_backends()` merges user YAML from `~/.config/needle/bead-backends/`, overriding built-ins by name. `bead_cli.backend` in config names one; `auto` detects.

A fourth CLI — `bd`, a fork, a future tool — is a YAML file. No recompile, no NEEDLE release.

### 2. Structural divergence is expressed as a *strategy* per operation, not as argv alone

This is the part a flat argv table cannot do, and the reason the first two drafts of this ADR reached for hardcoded impls. The backends differ in **which operations are single commands at all**: `bf` claims via one transactional `batch` because it has no `update --assignee`; `br` and `bead` claim via a plain `update`; `br` has no server-side `claim`, so `claim_auto` there is a two-step read-then-write with different race properties.

So each operation declares a strategy drawn from a small closed set, parameterized by argv templates:

| Operation | Strategies | `br` | `bf` | `bead` |
|---|---|---|---|---|
| `claim` | `compare_and_set` \| `batch_op` | compare_and_set | batch_op | compare_and_set |
| `claim_auto` | `atomic_subcommand` \| `non_atomic_scan` | non_atomic_scan | atomic_subcommand | atomic_subcommand |
| `split` | `transactional_batch` \| `sequential` | sequential | transactional_batch | sequential |
| create → ID | `bare_id` \| `json_field` | bare_id | bare_id | bare_id |
| labels | `csv` \| `repeated` | csv | repeated | repeated |
| `import` | `bare` \| `input_plus_mode` | bare | bare | input_plus_mode |

Six small enums cover every divergence found across three upstreams. NEEDLE implements each strategy **once**; a descriptor selects among them. A new CLI that fits the existing strategies is pure configuration. A new CLI that genuinely does something novel adds *one enum variant*, not a `BeadStore` impl — and that variant is then available to every backend.

Illustrative descriptor (abridged):

```yaml
name: beads_rust
binary: br
detect_paths: ["~/.cargo/bin/br"]
identity_pattern: "^br "          # guards against the shim — see §3
operations:
  show:       { argv: ["show", "{id}", "--json"], parse: json_object }
  create:     { argv: ["create", "--title", "{title}", "--body", "{body}", "--silent"],
                labels: { style: csv, flag: "--labels" }, parse: bare_id }
  claim:      { strategy: compare_and_set,
                argv: ["update", "{id}", "-s", "in_progress", "--assignee", "{actor}"] }
  claim_auto: { strategy: non_atomic_scan }
  dep_add:    { argv: ["dep", "add", "{blocked}", "{blocker}", "-t", "blocks"] }
  split:      { strategy: sequential }
capabilities: { atomic_claim: false, transactional_batch: false, velocity_metadata: false }
```

The `bf` descriptor differs only in data — `claim: { strategy: batch_op }`, `dep_add: ["dep","add","{blocker}","--blocks","{blocked}"]`, `capabilities.atomic_claim: true` — not in code.

### 3. Identity is verified, never inferred

Every descriptor carries `identity_pattern`, checked against the resolved binary's `--version` output. `~/.local/bin/br` is a shim that `exec`s `bf`, so it reports `bf <version>` and fails the `beads_rust` descriptor's `^br ` check. This is the mechanism that makes §5's chimera impossible to reconstruct: a store can never bind to a binary speaking a different dialect, because binding requires the identity to match the descriptor that supplies the argv.

### 4. Capabilities are declared and optionally probed

Descriptors declare capabilities; where an upstream exposes a machine-readable contract (`bead capabilities --profile`, `bf schema`/`robot-docs`, `br schema`), probe at discovery and **reconcile against the declaration**, warning on mismatch. A declaration that drifts from reality is then visible rather than silently wrong — the failure mode of the bf 0.2.0 `--limit 0` workaround, which is still applied unconditionally at `:1357`/`:2094`.

### 5. One `CliBeadStore`

A single `BeadStore` impl driven by a `BeadBackend`. `BrCliBeadStore` and `BfCliBeadStore` are deleted; their behavior survives as the `beads_rust` and `bead-forge` builtin descriptors. This removes the chimera by construction rather than by repair: there is no store that can hold beads_rust argv and a bf binary at the same time.

### 6. The rest follows

- **One resolver.** `resolve_bead_cli()` returns a descriptor plus a verified path, replacing all five hardcoded chains (`:758-775`, `:1127-1136`, `:1893-1902`, `worker/mod.rs:732-742`, `cli/mod.rs:3626-3632`). Per-workspace via `.needle.yaml`.
- **`predispatch` goes through the store** (`validation/predispatch.rs:128` → `BeadStore::show`), removing the last non-trait call site.
- **Prompt fragments render from the active descriptor** (`prompt/mod.rs:56`, `:64`, `:294-325`), so the agent is told to run the binary that is present and the dialect is written down exactly once — in the descriptor.
- **`needle bead-backend <name>`**, mirroring the existing `needle test-agent`: resolve, verify identity, probe capabilities, print the resolved argv for each operation. A new descriptor is testable without dispatching a worker.

### 7. Backend priority: bead-rs primary, bead-forge secondary, beads_rust tertiary, open world beyond

Operator decision, 2026-08-12. Three consequences, none of which change the
mechanism above — the descriptor design was already priority-agnostic, which is
why this is a sequencing and defaults decision rather than a redesign.

**Sequencing inverts.** Phase 16 currently authors descriptors 16.4a beads_rust →
16.4b bead-forge → 16.4c bead-rs, so the primary backend lands last. Reorder to
bead-rs first. Its descriptor is also the only one that cannot be written from a
released artifact — bead-rs is Forgejo-only with no GitHub releases and is under
active development in agent-sandbox, so its `verified_against` names a commit and
must be re-verified when that moves.

**`auto` detection prefers `bead`, then `bf`, then `br`.** Today every discovery
chain resolves `bf` first, which is §5's defect in a different costume: the
default should express the intended ordering rather than whatever the fleet host
happens to have installed.

**The primary backend is the weakest on two capabilities, and that must be
declared rather than discovered in production.** Re-probed against `bead` 0.1.1
on 2026-08-12 (the ADR's original evidence was `fa30574`, since superseded):

- **No transactional `batch`.** `bf` remains the only backend that can make
  mitosis atomic. With bead-rs primary, `split` runs the `sequential` strategy
  for the default backend, so a SIGKILL, OOM, or pod eviction between child
  creation and dependency wiring leaves orphaned children — the exact hazard
  `bead_store` already documents for the non-atomic path. This is a real
  regression against the current bf default and belongs in 16.11's operator-facing
  capability-gap doc, not in a log line.
- **No velocity metadata on `claim`.** `bf claim` takes `--model --harness
  --harness-version`; `bead claim` does not. Telemetry that NEEDLE records today
  is simply absent for the primary backend.

Against that, bead-rs is *stronger* where it matters most for fleet correctness,
and the priority decision buys these:

- **Fenced, TTL'd leases** — `claim --lease-ttl --renew-lease --fencing-token`.
  This is a direct mechanism for the orphaned-claim class tracked in `bf-1e0`
  ("Self-healing fleet: recover from worker death, exhaustion & orphaned
  claims"); four NEEDLE beads were found on 2026-08-12 still assigned to workers
  that no longer exist. A TTL expires the claim without a janitor, and the
  fencing token stops a resurrected worker from writing after its lease lapsed.
- **Cycles rejected at insertion**, inside the transaction — the defect class
  that required a manual graph repair of this very workspace on 2026-08-12.
- **Readiness derived from the edge set**, with no stored `blocked` status to go
  stale, and the unfinished-blocker test embedded in the eligibility query so
  `ready` cannot disagree with the graph.
- `bead why` and `claim --why`, giving reason codes the other two cannot produce.

**Two axes of interoperability, only one of which is on this critical path.**
Worth separating because they are easy to conflate:

- *NEEDLE → any bead CLI* — this ADR. Needs descriptors and nothing else.
- *bead-rs ↔ other implementations' stored data* — bead-rs F012 profiles
  (`br-v1`, `bf-v1`), which `bead capabilities` currently reports as absent and
  which its own plan records as externally blocked on independently approved
  fixtures.

Making bead-rs NEEDLE's primary backend **does not depend on F012**. NEEDLE
drives a CLI; it does not ask one backend to read another's store. Only
migrating a workspace between backends needs the profiles.

**The open-world row is the requirement that fixes the design.** "Other bead
systems that exist in the world" is not satisfied by three good descriptors — it
is satisfied only if a fourth backend needs no NEEDLE change. That is the property
§1 and §2 deliver, and it is the reason the hardcoded-impl-per-upstream drafts are
rejected rather than merely inelegant. Two obligations follow: the six strategy
enums must be treated as a published extension point rather than an internal
detail, and capability negotiation (§4) is the discovery mechanism for a backend
nobody wrote a descriptor for — all three current backends already expose a
capabilities-style surface, and bead-rs's is a full JSON contract
(`contract: native-v1`, `atomic_claim`, statuses, checkpoint formats, schema refs).

Minor defect found while re-probing: `bead capabilities` reports
`"version": "0.1.0"` while `bead --version` reports `0.1.1`. A descriptor's
`verified_against` must read one of these deliberately, and the disagreement
should be filed upstream.

## Alternatives Considered

- **One hardcoded `BeadStore` impl per upstream** (this ADR's first two drafts). Rejected on the operator's requirement: it hardcodes three harnesses, so a fourth bead CLI needs a Rust file, a code review, and a NEEDLE release. It also duplicates the ~20 operations three times, which is how the two existing impls drifted apart in the first place — `add_dependency` is correct in one and wrong in the other precisely because the same command is written twice. The strategy-enum design above answers the objection those drafts raised (that a flat argv table cannot express structural difference) without paying the duplication cost.

- **A flat argv-template table with no strategies.** Rejected, and this was the correct objection in the earlier drafts — it just did not follow through to the fix. A table of argv strings cannot express "claim is one atomic call here and a read-then-write there", and papering over that would silently give beads_rust workspaces the race semantics of bead-forge. Strategies make the difference explicit and testable.

- **Standardize on one CLI and drop the others.** Rejected on the operator's explicit requirement: all three upstreams are live and independently maintained. Note the `br` shim is itself part of the hazard — it makes `which br` return a binary that speaks `bf`, which is how §5 went unnoticed.

- **A `bf`-compatible shim in the sandbox image translating to `bead`.** Rejected: buries the mismatch in an undocumented script on one image — the same shape as the shim that concealed §5 — and cannot express `batch`-shaped calls, since bead-rs has no equivalent.

- **Teach the other CLIs a `batch` subcommand so one dialect works everywhere.** Scope inversion: makes two external roadmaps a blocker for NEEDLE portability, and leaves every other divergence unaddressed.

- **Full scripting (Lua/Rhai/WASM) per backend.** Genuinely maximal configurability. Rejected as disproportionate: six enums cover every divergence observed across three independently-evolving upstreams plus their common Go ancestor, and a scripting runtime turns every bead operation into arbitrary user code on the path that claims and closes work.

## Consequences

- **A new bead CLI is a YAML file.** The three current backends stop being privileged: they are the shipped defaults, overridable by name like any agent adapter. This is the property the operator asked for and the first two drafts did not deliver.
- **The chimera becomes unconstructible.** Argv and binary come from the same descriptor, and identity is verified against it. §5's failure mode cannot recur by omission.
- **Duplication collapses.** ~20 operations currently written twice (soon three times) become one engine plus three data files. The `add_dependency` split-brain — right in one impl, wrong in the other — cannot happen when the command is written once per backend and nowhere else.
- **Atomic mitosis stays bf-only and atomic claim stays bf/bead-only.** The descriptor makes these gaps *declared and visible* rather than implicit in which impl you happened to construct, but it does not close them. `br` retains a real TOCTOU window between `ready` and `update` in `claim_auto` — two workers on one beads_rust workspace can claim the same bead. This is the duplicate-claim hazard CLAUDE.md already names as the real fleet failure mode, and it must be surfaced to operators, not just logged.
- **Strategy coverage is a real constraint.** A backend that fits no existing strategy needs an enum variant before its YAML works. That is the honest cost of not shipping a scripting runtime, and the failure is loud (unknown strategy → descriptor validation error) rather than silent.
- **Descriptor validation becomes load-bearing.** A malformed descriptor is now the way a fleet breaks. `load_bead_backends()` must reject unknown strategies, unresolvable placeholders, and missing required operations at load time, not at first claim — and `needle bead-backend <name>` must make that checkable before deployment.
- **The two existing impls are deleted, not refactored.** Their test suites must be re-expressed as descriptor conformance tests against the same fixture-CLI pattern (`:2809`, `:2858`, `:2960`), or coverage silently drops on the paths where a bug means duplicate dispatch or a lost bead.

## Evidence

- Installed binaries probed 2026-08-11: `~/.cargo/bin/br` → `br 0.1.28` (dicklesworthstone/beads_rust); `~/.local/bin/bf` → `bf 0.4.1` (jedarden/bead-forge); `~/go/bin/bd` → `bd 0.49.6` (Go beads, ancestor). `~/.local/bin/br` is a **shim** that logs the caller and `exec`s `bf` — so `which br` does not find beads_rust, which is how §5 stayed hidden.
- `br 0.1.28 create --help`: `-d, --description [aliases: --body]`, `-l, --labels` (comma-separated), `--silent`, `--json`. `br dep add --help`: `Usage: br dep add [OPTIONS] <ISSUE> <DEPENDS_ON>`, `-t, --type … [default: blocks]`. `br update --help`: `-s, --status`, `--assignee`, `--notes`. `br sync --help`: `--flush-only`, `--import-only` (both bare). `br --help`: no `claim`, no `batch`; has `ready`, `schema`.
- `bf 0.4.1 create --help`: `--title --description --type --priority --assignee --label --json --envelope --no-auto-flush --no-progress --workspace` — no `--body`, `--silent`, or `--labels`. `bf dep add --help`: `Usage: bf dep add [OPTIONS] <BLOCKER>` plus `--blocks <BLOCKS>`. Verified empirically: `bf dep add bf-46m05 --blocks bf-2qm6r` → `Added dependency: bf-2qm6r depends on bf-46m05 (blocks)`. `bf --help`: has `ready`, `claim`, `batch`, `schema`, `robot-docs`.
- `bead` 0.1.0 surface from `~/bead-rs/src/cli.rs` @ `fa30574`: `Command` enum `:47-131` (no `batch`), `CreateOptions` `:181` (`--description`, repeated `--label`, no `--json`), `ListOptions` `:241` (`--json`, `--ready`, `--limit` 0-999999), `ClaimOptions` `:370` (no model/harness flags), `UpdateOptions` `:444` (**has** `--assignee` and `--clear-assignee`), `SyncCommand` `:667` (`--import-only` requires `--input` + `--restore-into-empty`|`--merge`), `DepAddOptions` `:975` (`--kind`), `DoctorOptions` `:1059`, `CapabilitiesOptions` `:1116`.
- NEEDLE call sites: resolution `:758-775`, `:1127-1136`, `:1893-1902`, `worker/mod.rs:732-742`, `cli/mod.rs:3626-3632`, `validation/predispatch.rs:128`. beads_rust-dialect argv in `BrCliBeadStore`: `:1357`, `:1550-1563`, `:1579`, `:1675`, `:1706`. Grafted bf-only paths in the same store: `:1194-1220` (`bf claim`), `:1428-1437` (claim via `batch`), `:1479-1486` (release via `batch`), `:1485`/`:1508` (split via `batch`). Genuine argv bug in the bf store: `:2288`.
- Store construction: `BrCliBeadStore` at `worker/mod.rs:1385`, `supervisor/mod.rs:214`, `strand/explore.rs:71`/`:450`, `strand/splice.rs:747`/`:839`/`:999`; `BfCliBeadStore` only at `cli/mod.rs:997`, `:3384`.
- Why bf needs `batch` for claim: commit `ce0134c` ("route claim/release/clear_assignee through bf batch, not bf update --assignee", bead `bf-1hmey`) — bf 0.4.1 removed `--assignee` from `update`.
- Prompt-embedded dialect: `prompt/mod.rs:56`, `:64`, `:294-325`, `:1066-1067`. `:325` says `bf dep add <blocker> --blocks <blocked>` — correct for `bf`, wrong for `br` and `bead`, which is exactly the failure mode of writing a dialect down in a second place.
- bead-forge-specific handshake: `:295-419`, and the unconditional `--limit 999999` workaround it justifies at `:1357`, `:2094`.

## Revision History

- **2026-08-11, draft 1.** Framed the task as "add bead-rs support", treating `br` as a deprecated alias of `bf` rather than a live upstream. Diagnosed `BrCliBeadStore`'s `--body`/`--silent`/`--labels` and two-positional `dep add` as stale drift to be rewritten toward the bf dialect.
- **2026-08-11, draft 2** — after the operator required first-class support for all three upstreams. Probing the real `~/.cargo/bin/br` (which `which br` does not find, because a shim shadows it) showed that argv is **correct beads_rust**, and the actual defect is `discover()` binding that store to the `bf` binary, plus bf-only `batch`/`claim` calls grafted into it. Draft 1's proposed fix would have broken genuine beads_rust support. Only one real argv bug survived: `BfCliBeadStore::add_dependency` (`:2288`).
- **2026-08-11, draft 3 (current)** — after the operator required the bead-CLI component be genuinely configurable rather than hardcoding harnesses for three named CLIs. Drafts 1 and 2 both landed on one Rust impl per upstream, and draft 2 explicitly argued against a data-driven design on the grounds that an argv table cannot express structural divergence. That objection was right about argv tables and wrong about the conclusion: a per-operation *strategy* enum parameterized by argv templates expresses the divergence exactly, and NEEDLE already runs this pattern for agent harnesses (`AgentAdapter` + `load_adapters`, `src/dispatch/mod.rs:570-660`) — a precedent both earlier drafts missed while proposing a parallel mechanism for the same problem one layer down.
- **2026-08-12, draft 4** — after the operator set the interop goal explicitly: **bead-rs primary, bead-forge secondary, beads_rust tertiary, and other bead systems that exist in the world.** The mechanism in drafts 1-3 needed no change; the data-driven design was already priority-agnostic, which is the evidence that draft 3 chose correctly. What changed is §7: descriptor authoring order inverts so the primary backend is written first rather than last, `auto` detection prefers `bead` over the incumbent `bf`, and the primary backend's two capability gaps (no transactional `batch`, no velocity metadata on claim) become declared operator-facing facts instead of surprises — non-atomic mitosis is a real regression against today's bf default. Re-probed `bead` 0.1.1 rather than trusting draft 3's `fa30574` evidence, which surfaced the fenced-lease claim surface (`--lease-ttl`/`--renew-lease`/`--fencing-token`) that drafts 1-3 did not know existed and that bears directly on `bf-1e0`. Also separated the two interop axes: NEEDLE→CLI (this ADR) does not depend on bead-rs F012 profiles, which `bead capabilities` reports as absent and which bead-rs's own plan records as externally blocked. The open-world row is promoted from an incidental benefit to a stated requirement, which makes the six strategy enums a published extension point rather than an internal detail.
