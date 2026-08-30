# tests/pending

Integration tests whose subject does not exist yet. Cargo only auto-discovers
`tests/*.rs`, so files here are kept but not built. Move a file back up to
`tests/` in the same commit that lands its API.

- `orphaned_bead_recovery_test.rs` — calls `Worker::run_cycle()`, which does not
  exist (`Worker::run` is the only public entry point). Owner: bead
  needle-6d76f548 (orphaned bead recovery on worker boot). Parked 2026-08-30
  because it broke `cargo test --no-run` for the whole crate.
