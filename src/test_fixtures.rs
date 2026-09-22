//! Unique, non-shared roots for test fixtures.
//!
//! Tests must never name a shared or predictable directory (`/tmp`,
//! `/tmp/test`, `/tmp/workspace`, …) as a bead workspace or any other
//! fixture root. Any test that materializes files there collides with every
//! other test that does the same — across threads, test binaries and CI
//! lanes — and code under test that writes `<root>/.beads` can poison
//! bead-rs workspace discovery for the whole machine (bead needle-bff53d4c,
//! incident 2026-08-30).
//!
//! Every call hands back a fresh, uniquely-named directory under `$TMPDIR`.
//! The directory is intentionally *kept* rather than deleted on drop:
//! fixture values flow into configs and structs that outlive any single
//! binding, and the per-target `TMPDIR` that `scripts/definition-of-done.sh`
//! gives each test lane (and the OS, eventually) reaps the litter.

use std::path::PathBuf;

/// Create a unique fixture root directory and return its path.
///
/// The directory exists for the life of the process; the path is unique
/// across processes, so two tests calling this with the same tag never
/// share a directory.
pub(crate) fn fixture_root(tag: &str) -> PathBuf {
    match tempfile::tempdir() {
        Ok(dir) => dir.keep(),
        Err(e) => panic!("fixture_root({tag}): failed to create temp dir: {e}"),
    }
}

/// [`fixture_root`] as a `String`, for fixtures typed as strings (CLI argv,
/// JSON payloads, template fields).
pub(crate) fn fixture_root_str(tag: &str) -> String {
    fixture_root(tag).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_root_is_unique_per_call() {
        let a = fixture_root("uniq");
        let b = fixture_root("uniq");
        assert_ne!(a, b, "two calls with the same tag must not share a dir");
        assert!(a.is_dir() && b.is_dir());
        assert!(a.starts_with(std::env::temp_dir()));
    }

    #[test]
    fn fixture_root_str_round_trips() {
        let s = fixture_root_str("str");
        assert!(PathBuf::from(&s).is_dir());
    }
}
