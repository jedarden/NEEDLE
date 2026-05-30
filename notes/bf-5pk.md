# Bead bf-5pk: needle init subcommand

## Task Summary

Add `needle init` subcommand for v1→v2 migration and config bootstrapping.

## Finding

The `needle init` command is **already fully implemented** in the codebase.

## Implementation Location

- `src/cli/mod.rs` lines 209-215: `Init` variant in `CliCommand` enum
- `src/cli/mod.rs` lines 1133-1520: `cmd_init()` function implementation
- `src/cli/mod.rs` line 377: Command wired up in `run()` function

## Acceptance Criteria Status

All criteria met:

1. ✅ `needle init` creates `~/.config/needle/config.yaml` when none exists
   - Implemented at line 1143: `let config_path = dirs_or_home(".config/needle/config.yaml");`
   - Config creation at lines 1486-1494

2. ✅ `needle init` detects v1 artifacts and migrates compatible settings
   - v1 detection at lines 1162-1206
   - Migrates: agent name (lines 1167-1181), workspace path (lines 1183-1205)

3. ✅ `needle init` is idempotent (safe to run on already-initialized installs)
   - Check at line 1147: `if config_path.exists()`
   - Shows current config and returns early if already initialized

4. ✅ `needle --help` lists `init` in the Commands section
   - Help text defined at lines 210-215
   - Verified: `needle --help` shows `init` command

## Verification

```bash
$ ./target/release/needle --help | grep init
  init          Initialize v2 config with optional v1 migration

$ ./target/release/needle init --help
Initialize v2 config with optional v1 migration.

Creates ~/.config/needle/config.yaml. Detects existing v1 artifacts
in ~/.needle/ and migrates compatible settings (agent name, workspace
path, worker count) to the v2 YAML schema. Safe to run on already-
initialized installs (idempotent).

$ ./target/release/needle init  # (when config exists)
Config already exists: /home/coding/.config/needle/config.yaml
  Agent default: hooks
  Workspace: /home/coding/zai-proxy
  Max workers: 4
```

## Conclusion

The task requirements are already satisfied by the existing code. No implementation changes needed.
