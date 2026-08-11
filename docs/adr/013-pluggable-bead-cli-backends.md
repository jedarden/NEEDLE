# ADR-013: Pluggable Bead-CLI Backends — Three Upstreams, One Configurable Seam

**Status:** Proposed — 2026-08-11 (revised same day; see Revision History)
**Deciders:** operator (jedarden), via Claude Code
**Tracking:** see Phase 16 in `docs/plan/plan.md`; beads filed under label `bead-cli-backend`

## Context

NEEDLE must drive **three** independently-evolving bead CLIs, not one canonical tool plus legacy debris:

| Backend | Binary | Upstream | Installed here |
|---|---|---|---|
| **beads_rust** | `br` | `github.com/dicklesworthstone/beads_rust` | `~/.cargo/bin/br`, v0.1.28 |
| **bead-forge** | `bf` | `git.ardenone.com/jedarden/bead-forge` | `~/.local/bin/bf`, v0.4.1 |
| **bead-rs** | `bead` | `git.ardenone.com/jedarden/bead-rs` | agent-sandbox `needle-pod`, v0.1.0 |

(A fourth, the original Go `beads` — `bd` v0.49.6, `~/go/bin/bd`, the common ancestor of all three — is out of scope for this ADR but is why the three dialects rhyme without matching.)

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
| machine-readable contract | `schema` | `schema`, `robot-docs` | `capabilities --profile` |

Two structural facts fall out. **`bf` is the only backend with a transactional `batch`**, so it is the only one that can make mitosis atomic. **`bf` is the only backend that dropped `update --assignee`**, so it is the only one that *needs* `batch` for something as ordinary as claiming. Those two facts are the same fact, and together they are why a shared "argv template table" cannot work: the backends do not differ in flag spelling, they differ in which operations are single commands at all.

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

## Decision

**One `BeadStore` impl per upstream, selected by configuration, with capability negotiation per backend.** Three real backends, three adapters, no shared argv template.

1. **`BrCliBeadStore` becomes honestly beads_rust-only.** Keep its existing create/dep/sync/list argv — already correct. Remove the grafted `bf batch` and `bf claim` paths and the second resolver at `:1127`. `claim`/`release`/`clear_assignee` use `update -s <status> --assignee …`, which beads_rust has. `claim_auto` has no beads_rust equivalent: implement it as the existing non-atomic `ready` → `claim(id)` sequence and log the downgrade. `split_bead` inherits the non-atomic trait default.

2. **`BfCliBeadStore` becomes the only store that uses `batch`.** It is the only backend with one, and the only one that needs it (no `update --assignee` since 0.4.1). Fix `add_dependency` (`:2288`) to `dep add <blocker> --blocks <blocked>`. It keeps velocity-aware `claim --model/--harness/--harness-version` and the atomic `split_bead` override — both bf-exclusive.

3. **`BeadCliBeadStore`** — new, for bead-rs. `update --assignee`/`--clear-assignee` for claim and release (bead-rs kept what bf dropped). `create --description` + repeated `--label`, parsing the bare-ID stdout. `dep add <blocked> <blocker> --kind blocks`. `full_rebuild` supplies the `--input` and `--restore-into-empty` that bead-rs requires. `claim_auto` uses `claim --assignee --json`, omitting metadata flags bead-rs does not accept. No `batch`, so no atomic split.

4. **Split `bead_store/mod.rs`** (3440 lines, two impls, soon three). `mod.rs` keeps the trait, shared types, parse helpers, ETXTBSY spawn retry, corruption detection; `br_cli.rs`, `bf_cli.rs`, `bead_cli.rs` take one impl each.

5. **One resolver, one config knob.** `BeadCliConfig` in `src/config/mod.rs` (alongside `WorkspaceConfig` at `:289`): `backend` (`auto` | `br` | `bf` | `bead`) and optional explicit `path`. A single `resolve_bead_cli()` replaces all five hardcoded chains and returns *both* the backend identity and the path, so a store can never again be bound to a binary that speaks a different dialect. **`auto` must resolve backend and binary together** — the current bug is precisely that these were decided independently. Per-workspace via `.needle.yaml`, so a fleet spanning backends needs no per-host global config.

6. **Capability negotiation per backend, not a hardcoded matrix.** Each upstream ships a machine-readable contract surface — `bead capabilities --profile`, `bf schema` / `bf robot-docs`, `br schema`. Probe at discovery and record on the store; gate optional paths (notably atomic split and atomic claim) on the result rather than a table that goes stale the way the bf 0.2.0 `--limit 0` workaround did. Make the bead-forge version handshake backend-conditional.

7. **`predispatch` goes through the store** (`src/validation/predispatch.rs:128` → `BeadStore::show`), removing the last non-trait bead-CLI call site.

8. **Prompt fragments are backend-derived** (`prompt/mod.rs:56`, `:64`, `:294-325`), so the agent is told to run the binary that is present, and the dialect is written down exactly once.

## Alternatives Considered

- **A `BeadDialect` table of argv templates behind one generic `CliBeadStore`.** This is what "make it configurable" suggests and it is wrong. The backends differ *structurally*: `bf` claims with one `batch` call because it has no `update --assignee`; `br` and `bead` claim with a plain `update`; `br` has no server-side `claim` at all, so `claim_auto` is a two-step read-then-write there and a single atomic call elsewhere — with completely different race properties. A template table would have to carry behavior, not strings, at which point it is a trait again with worse ergonomics and no compile-time exhaustiveness. Rejected.

- **Standardize on one CLI and drop the others.** Tempting, and `br` is nominally deprecated here (`~/.local/bin/br` is a shim execing `bf`). Rejected on the operator's explicit requirement: all three upstreams are live, independently maintained, and deployed on different hosts. Making NEEDLE portable across them is the point, not an accident to be normalized away. Note also that the shim is itself part of the hazard — it makes `which br` return something that speaks `bf`, which is how the §5 chimera went unnoticed.

- **Ship a `bf`-compatible shim in the sandbox image translating to `bead`.** Fastest unblock, zero NEEDLE changes. Rejected: it buries the dialect mismatch in an undocumented shell script on one container image — the same shape as the `br` shim that concealed §5 — and it cannot express the `batch`-shaped calls at all, since bead-rs has no equivalent to translate into.

- **Teach bead-rs and beads_rust a `batch` subcommand so the bf store just works everywhere.** Scope inversion: makes two external roadmaps a blocker for NEEDLE portability, and still leaves `--body`/`--labels`/`--silent`, `dep add` arity, `claim` metadata, and `sync --import-only` arity unaddressed.

- **Autodetect with no config knob.** Rejected: this host has `br`, `bf`, and `bd` simultaneously, with a shim shadowing one of them. Silent autodetection makes "which store did that worker write to" unanswerable from config alone. `auto` stays the default; an explicit override must exist.

## Consequences

- NEEDLE drives all three upstreams, unblocking `game-of-life` in agent-sandbox and any future mixed-backend fleet.
- **`BrCliBeadStore` stops being silently broken.** Once bound to the binary it was written for, its argv is already correct — this is a repair by *unbinding*, not by rewriting argv. Any "fix" that rewrites `--body` → `--description` in that store is wrong and would break real beads_rust.
- **Atomic mitosis is bf-only.** `br` and `bead` have no transactional batch, so both inherit the non-atomic `split_bead` default and its documented orphaned-child crash window. Bounded, backend-scoped, and logged — not new risk on bf.
- **Atomic server-side claim is bf- and bead-only.** beads_rust has no `claim`, so its `claim_auto` is a read-then-write with a real TOCTOU window between `ready` and `update`. This matters for multi-worker fleets on beads_rust workspaces and must be documented where operators will see it, not just in code.
- **Velocity-aware claim scoring is bf-only.** `--model`/`--harness`/`--harness-version` exist on no other backend, so `worker_sessions`/`velocity_stats` rows simply are not written there.
- `bead_store/mod.rs` splits into four files; the public surface must be re-exported so no consumer outside the module changes.
- Three code paths through claim and release — the operations where a bug means duplicate dispatch or a lost bead. Each needs the fixture-CLI coverage the existing pattern (`:2809`, `:2858`, `:2960`) generalizes to, with argv assertions per backend.
- `needle doctor` can finally report *which* backend it resolved and why, replacing a message that is wrong on two of the three.

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

- **2026-08-11, initial draft.** Framed the problem as "add bead-rs support", treating `br` as a deprecated alias of `bf` rather than a live upstream. Diagnosed `BrCliBeadStore`'s `--body`/`--silent`/`--labels` and two-positional `dep add` as stale-drift bugs to be rewritten toward the bf dialect.
- **2026-08-11, revised** after the operator required first-class support for all three upstreams. Probing the real `~/.cargo/bin/br` (which `which br` does not find, because a shim shadows it) showed that argv is **correct beads_rust** and the actual defect is `discover()` binding that store to the `bf` binary — plus bf-only `batch`/`claim` calls grafted into it. The rewrite-toward-bf fix the first draft proposed would have broken genuine beads_rust support. Only one real argv bug survives, in `BfCliBeadStore::add_dependency` (`:2288`). Recorded here rather than silently amended: the first draft's diagnosis was confidently wrong in a way worth being able to trace.
