# ADR-013: Pluggable Bead-CLI Backends — Configurable Binary, Discoverable Dialect

**Status:** Proposed — 2026-08-11
**Deciders:** operator (jedarden), via Claude Code
**Tracking:** see Phase 16 in `docs/plan/plan.md`; beads filed under label `bead-cli-backend`

## Context

The `game-of-life` project runs in the **agent-sandbox** cluster (`needle-pod` namespace) against **bead-rs**, whose CLI binary is `bead` — not `bf` (bead-forge) and not the deprecated `br` shim. Stock NEEDLE cannot drive it. A worker booted there fails at the first claim.

This is not a missing feature so much as an abstraction that was drawn one layer too shallow. `trait BeadStore` (`src/bead_store/mod.rs:546`) already exists, is already `Send + Sync`, and is already what every consumer holds — `Arc<dyn BeadStore>` in `Worker` (`src/worker/mod.rs:306-308`), in `Claimer`, and in every strand. There are already two impls (`BrCliBeadStore` at `:701`, `BfCliBeadStore` at `:1852`). Nothing in the worker or strand layer knows or cares which CLI is underneath. That part of the design is sound and does not need to change.

What breaks is that four things leak past that seam.

### 1. Binary resolution is hardcoded in five places, two of which bypass the trait entirely

| Site | Behavior |
|---|---|
| `src/bead_store/mod.rs:758-775` | `BrCliBeadStore::discover` — fixed chain `bf` → `~/.local/bin/bf` → `br` → `~/.local/bin/br`, error text `"bead CLI not found; install bead-forge (bf)"` |
| `src/bead_store/mod.rs:1127-1136` | a **second**, independent `resolve_bf()` used only by `run_bf_batch` / `run_bf_claim` |
| `src/bead_store/mod.rs:1893-1902` | `BfCliBeadStore::discover` — a third copy of the same chain |
| `src/worker/mod.rs:732-742` | boot-time version handshake, `which::which("bf")` inline |
| `src/cli/mod.rs:3626-3632` | `needle doctor` preflight — `bf` else `br` else fail `"no bead store CLI found on PATH (checked bf, br)"` |
| `src/validation/predispatch.rs:128` | `run(workspace, "bf", &["show", bead_id, "--json"])` — **bypasses `BeadStore` altogether** |

Five sites, four spellings of the same fallback chain, and one caller that does not go through the trait at all. Even if the store were configurable today, `predispatch` would still shell out to a literal `"bf"`.

### 2. The *dialect* — not just the binary name — is baked into `BrCliBeadStore`'s method bodies

This is the substantive work. `BrCliBeadStore` is not "a CLI-backed store"; it is "a bf-dialect store". Against bead-rs `bead` (surface enumerated from `~/bead-rs/src/cli.rs`, `Command` enum at `:47-131`):

| NEEDLE emits | bead-rs `bead` | Outcome |
|---|---|---|
| `batch --json '[{"op":"update",…}]'` — used for **claim** (`:1428-1437`), **release** (`:1479-1486`), and **split** (`:1485`, `:1508`) | **no `batch` subcommand exists** | hard fail on the hottest path |
| `claim --model X --harness Y --harness-version Z --assignee A --json` (`:1206-1220`) | `ClaimOptions` (`cli.rs:370`) accepts only `--assignee/--json/--why/--policy/--lease-ttl/--renew-lease/--fencing-token` | clap rejects unknown args, exit 2 |
| `create --title T --body B --json --silent --labels a,b` (`:1550-1563`) | `CreateOptions` (`cli.rs:181`) uses `--description`, repeated `--label`, and has **no `--json`** — it prints the bare issue ID on stdout | fail (**and already fails on `bf` — see Addendum**) |
| `dep add <blocked> <blocker> --type blocks` (`:1579`) | `DepAddOptions` (`cli.rs:975`) spells it `--kind blocks` | fail |
| `sync --import-only` bare (`:1706`) | requires `--input` **and** exactly one of `--restore-into-empty` / `--merge` (`SyncCommand`, `cli.rs:667`) | fail |
| `list --json --limit 999999` (`:1357`), `show --json`, `update --status`, `reopen`, `close --reason`, `label add/remove --label`, `doctor [--repair]`, `sync --flush-only` | all present and compatible (`ListOptions` `cli.rs:241` caps `--limit` at 999999; `--ready` also available) | works |

Note the direction of the divergence at claim/release. NEEDLE routes those through `bf batch` **only because** bf 0.4.1 dropped `--assignee` from `update` (ADR-referenced bead `bf-1hmey`, commit `ce0134c`). bead-rs's `UpdateOptions` (`cli.rs:444`) *has* both `--assignee` and `--clear-assignee`. So the bead-rs implementation of claim and release is not harder than bf's — it is the straightforward `update` call that bf used to support and no longer does. The two dialects diverged in opposite directions from a common ancestor.

`split_bead` is the one place where bead-rs is genuinely weaker: with no `batch`, it cannot commit N creates plus N dependency links in one transaction, so it must fall back to the trait's existing non-atomic default (`src/bead_store/mod.rs:646-668`) and accept the orphaned-child crash window that default already documents.

### 3. The prompt hands the agent the dialect as literal text

`src/prompt/mod.rs` embeds `bf update` (`:56`), `bf close` (`:64`), and a whole `bf create` / `bf show` / `bf dep add` / `bf label add` block in the mitosis-split instructions (`:294-325`). A test asserts on the literal string (`:1066-1067`). Even with a perfect store abstraction, the agent inside the sandbox is told to run a binary that is not installed there.

There is already evidence this string lives in the wrong place: `:325` instructs `bf dep add <blocker-id> --blocks <blocked-id>`, while the store at `bead_store/mod.rs:1579` emits `dep add <blocked> <blocker> --type blocks`. The two copies of the dialect disagree, unnoticed, because nothing ties them together — and as the Addendum records, it is the **store** that is wrong here, not the prompt.

### 4. Version-handshake logic is bead-forge-specific

`check_bead_forge_version` / `run_version_handshake` (`src/bead_store/mod.rs:295-419`) parse bead-forge version strings to detect bf 0.2.0's `--limit 0` bug. Run against `bead --version` this degrades safely to `VersionCheck::Failed` and a warning, but the `--limit 999999` workaround it exists to justify is applied unconditionally at `:1357` and `:2094` regardless of backend.

## Decision

**Add a third `BeadStore` implementation; do not genericize the existing two.** Make the binary and backend selection configurable, and use the backend's own capability report to decide which optional paths are available.

1. **`BeadCliBeadStore`** — a new impl of the existing `BeadStore` trait speaking bead-rs dialect (`src/bead_store/bead_cli.rs`). Claim and release become plain `update --status … --assignee …` / `update --status open --clear-assignee` calls. `create` uses `--description` and parses the bare-ID stdout rather than JSON. `dep add` uses `--kind`. `split_bead` is **not** overridden — it inherits the non-atomic trait default. `full_rebuild` supplies the required `--input` and `--restore-into-empty` arguments that bead-rs's `sync --import-only` demands.

2. **Split `bead_store/mod.rs`.** At 3440 lines carrying two impls, adding a third to the same file is not defensible. `mod.rs` keeps the trait, shared types, parsing helpers, the ETXTBSY spawn retry, and corruption detection; `br_cli.rs`, `bf_cli.rs`, and `bead_cli.rs` each take one impl.

3. **One resolver, one config knob.** A new `BeadCliConfig` (`src/config/mod.rs`, alongside `WorkspaceConfig` at `:289`) with a `backend` field (`auto` | `bf` | `br` | `bead`) and an optional explicit `path`. A single `resolve_bead_cli(&BeadCliConfig) -> Result<(Backend, PathBuf)>` replaces all five hardcoded chains above. `auto` preserves today's precedence (`bf` → `~/.local/bin/bf` → `br` → `~/.local/bin/br`) and appends `bead` → `~/.local/bin/bead` → `/usr/local/cargo/bin/bead` so existing installs are unaffected. Configurable per workspace via `.needle.yaml`, so a fleet spanning both backends works without per-host global config.

4. **`predispatch` goes through the store.** `src/validation/predispatch.rs:128` stops shelling out to a literal `"bf"` and calls `BeadStore::show` instead. This is a strict simplification — it is already fetching exactly what `show` returns.

5. **Capability probe, not a version matrix.** bead-rs ships `bead capabilities --profile <p>` (`cli.rs:1116`). Probe it once at discovery and record the result on the store, rather than hardcoding a backend→features table that goes stale the way the bf 0.2.0 workaround did. Backends without a `capabilities` subcommand (bf, br) fall back to today's version handshake, which becomes backend-conditional rather than unconditional.

6. **Prompt fragments become backend-derived.** The bead-command block in `src/prompt/mod.rs:294-325` and the single-command references at `:56`/`:64` are generated from the resolved backend's dialect rather than hardcoded, so the agent is told to run the binary that is actually present. Fixing the pre-existing `--blocks` / `--type` drift at `:325` falls out of this, because there will be exactly one place the dialect is written down.

## Alternatives Considered

- **A `BeadDialect` table of argv templates, with one generic `CliBeadStore` over it.** This is the shape the phrase "make it configurable" suggests, and it is the wrong one. Claim, release, and split differ *structurally* between backends, not lexically: one transactional `batch` call versus one or two sequential `update` calls, with materially different crash-safety guarantees (the atomic split exists specifically to close the orphaned-child window described at `:637-645`). A template table would have to carry behavior, not strings — at which point it is a trait again, but with worse ergonomics and no compile-time exhaustiveness. Rejected.

- **Ship a `bf`-compatible shim in the sandbox image that translates to `bead`.** Fastest possible unblock and requires zero NEEDLE changes. Rejected: it moves the dialect mismatch into an undocumented shell script on one container image, exactly the class of invisible per-host divergence that `reference_agent_rule_surfaces_ex44_lab` records as a recurring failure. It also cannot fix the `batch`-shaped calls, since there is no bead-rs equivalent to translate them into.

- **Teach bead-rs a `batch` subcommand so the existing `BfCliBeadStore` just works.** Rejected as scope inversion: it makes an external tool's roadmap a blocker for NEEDLE portability, and it would still leave `--body`/`--description`, `--type`/`--kind`, the `claim` metadata flags, and `sync --import-only`'s required arguments unaddressed. Worth doing on its own merits later; not the path to backend support.

- **Detect the backend purely by probing, with no config knob.** Rejected: a host can legitimately have both `bf` and `bead` on PATH (this workstation nearly does), and silent autodetection makes "which store did that worker actually write to" unanswerable from config alone. `auto` remains the default, but an explicit override must exist.

## Consequences

- NEEDLE can drive bead-rs workspaces, unblocking `game-of-life` in agent-sandbox and any future non-bead-forge deployment.
- **Mitosis loses transactional atomicity on bead-rs backends.** A crash between a child `create` and its `dep add` leaves an orphaned child and a parent that never unblocks — the exact failure the `batch` path was built to prevent. The trait default already implements and documents this fallback, so this is a known, bounded regression confined to the bead-rs backend, not new risk on bf.
- **Velocity-aware claim scoring is unavailable on bead-rs.** `--model`/`--harness`/`--harness-version` have no bead-rs equivalent, so the `worker_sessions`/`velocity_stats` rows those flags populate are simply not written. Claim ordering falls back to bead-rs's own `--policy` (default `fifo-v1`). Workers on bead-rs backends will not benefit from model/harness routing.
- `bead_store/mod.rs` splits into four files. Import paths change across the crate; the public `crate::bead_store::{BeadStore, BrCliBeadStore, BfCliBeadStore}` surface must be re-exported from `mod.rs` so no consumer outside the module needs editing.
- The prompt test at `src/prompt/mod.rs:1066-1067` — which asserts the literal string `"bf close needle-abc"` — must become backend-parameterized. Its comment ("bead-forge, not the deprecated br alias") documents intent that survives; the hardcoded binary name does not.
- Adds a third code path through claim/release, the two operations where a bug means duplicate dispatch or a lost bead. Every new path needs the same fixture-CLI coverage the existing two have (`src/bead_store/mod.rs:2809`, `:2858`, `:2960` use a fake-binary fixture pattern that generalizes).
- `needle doctor` gains the ability to report *which* backend it resolved and why, replacing today's `"checked bf, br"` message that is wrong on any host running bead-rs.

## Evidence

- `src/bead_store/mod.rs:546` (`trait BeadStore`), `:701` / `:1852` (the two existing impls), `:646-668` (the non-atomic `split_bead` default and its documented orphaned-child window), `:637-645` (why the atomic override exists).
- Hardcoded resolution: `src/bead_store/mod.rs:758-775`, `:1127-1136`, `:1893-1902`; `src/worker/mod.rs:732-742`; `src/cli/mod.rs:3626-3632`; `src/validation/predispatch.rs:128` (the trait-bypassing call).
- bf-dialect call sites: `:1206-1220` (claim metadata flags), `:1357` (`list --json --limit 999999`), `:1428-1437` (claim via `batch`), `:1479-1486` (release via `batch`), `:1485`/`:1508` (split via `batch`), `:1550-1556` (`create --body`), `:1579` (`dep add … --type blocks`), `:1706` (`sync --import-only`).
- Why claim/release use `batch` at all: commit `ce0134c`, "route claim/release/clear_assignee through bf batch, not bf update --assignee" (bead `bf-1hmey`) — bf 0.4.1 removed `--assignee` from `update`.
- bead-rs surface (`~/bead-rs`, commit `fa30574`, version 0.1.0): `src/cli.rs:47-131` (`Command` enum — no `batch` variant), `:181` (`CreateOptions`: `--description`, no `--json`), `:241` (`ListOptions`: `--json`, `--ready`, `--limit` 0-999999), `:370` (`ClaimOptions`: no model/harness flags), `:444` (`UpdateOptions`: **has** `--assignee` and `--clear-assignee`), `:667` (`SyncCommand`: `--import-only` requires `--input` plus `--restore-into-empty`|`--merge`), `:975` (`DepAddOptions`: `--kind`, not `--type`), `:1059` (`DoctorOptions`: `--repair` present), `:1116` (`CapabilitiesOptions`).
- Prompt-embedded dialect and its drift: `src/prompt/mod.rs:56`, `:64`, `:294-325` (`:325` says `dep add <blocker> --blocks <blocked>`, which is correct for bf 0.4.1; the store at `bead_store/mod.rs:1579` emits `dep add <blocked> <blocker> --type blocks`, which is not — see Addendum), `:1066-1067` (test asserting the literal binary name).
- bead-forge-specific handshake: `src/bead_store/mod.rs:295-419` (`VersionCheck`, `check_bead_forge_version`, `run_version_handshake`), and the `--limit 999999` workaround it justifies at `:1357` and `:2094`.

## Addendum (2026-08-11): `BrCliBeadStore::create_bead` is already broken against the installed `bf`

Enumerating `bf create`'s real flag set while filing this ADR's beads turned up a live bug that is not a portability concern at all.

`bf 0.4.1 create` accepts exactly `--title --description --type --priority --assignee --label --json --envelope --no-auto-flush --no-progress --workspace`. It has **no `--body`, no `--silent`, and no `--labels`**.

`BrCliBeadStore::create_bead` (`src/bead_store/mod.rs:1550-1563`) emits all three:

```
create --title T --body B --json --silent [--labels a,b]
```

`BfCliBeadStore::create_bead` (`:2267-2280`) gets it right — `--description` plus repeated `--label`. But `BfCliBeadStore` is constructed in only two places, both in `src/cli/mod.rs` (`:997`, `:3384`). **Every worker path uses `BrCliBeadStore`** — `worker/mod.rs:1385`, `supervisor/mod.rs:214`, `strand/explore.rs:71`/`:450`, `strand/splice.rs:747`/`:839`/`:999` — and its `discover` (`:758-775`) resolves `bf` *first*. So the store the fleet actually runs speaks the old `br` dialect at a binary that no longer accepts it.

Consequence: any path that creates a bead through the trait fails with a clap parse error (exit 2) on a current `bf` install. That is `create_bead` itself and, through the trait's default `split_bead` (`:646-668`), **all of mitosis child creation**. Split has presumably been failing silently-ish in fleet logs for as long as bf 0.4.1 has been deployed.

This is the same class of defect ADR-012 documented — code that compiles, lints, and has config and telemetry wired, but whose actual invocation is wrong — and the same root cause this ADR exists to remove: the dialect is written down in more than one place, and the copies drifted. It is filed as its own bead, ahead of the Phase 16 work, and sequenced before the module split so the two do not collide in the same file.

### `add_dependency` is broken in *both* stores, and the prompt was right all along

The same flag-enumeration pass found a second live defect. `bf 0.4.1 dep add` takes **one** positional:

```
bf dep add [OPTIONS] <BLOCKER> --blocks <BLOCKS>
```

Verified empirically while filing these beads — `bf dep add bf-46m05 --blocks bf-2qm6r` prints `Added dependency: bf-2qm6r depends on bf-46m05 (blocks)` and moves `bf-2qm6r` to `blocked`.

Both stores emit two positionals instead:

- `BrCliBeadStore::add_dependency` (`:1579`) — `dep add <blocked> <blocker> --type blocks`
- `BfCliBeadStore::add_dependency` (`:2288`) — identical

`-t, --type` does exist and defaults to `blocks`, so the flag is harmless; the extra positional is not. Unlike the `create_bead` defect, this one is not confined to the `br`-dialect store — the `bf` store has it too, so there is no correct copy in the codebase to copy from.

This also **reverses** the drift claim in §3 above. `src/prompt/mod.rs:325` tells the agent `bf dep add <blocker-id> --blocks <blocked-id>`, which is exactly right for bf 0.4.1. The prompt is the accurate copy of the dialect and the store code is the stale one. That inversion is itself the argument for §6: when the same command is written down in two places, being right in one of them is luck, and you cannot tell which copy to trust without going to the binary. Filed as a separate P1 bead, with an acceptance criterion requiring a real round-trip against a scratch workspace — getting the argument order backwards would invert every dependency edge, which is worse than failing loudly.
