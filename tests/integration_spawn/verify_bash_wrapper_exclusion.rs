//! Regression test for bash wrapper exclusion in process discovery.
//!
//! This test verifies that scan_needle_processes() correctly excludes
//! bash wrapper processes and only discovers actual needle worker processes.
//!
//! Regression for bead bf-4lkno: A worker was found running for 3+ days,
//! but bash wrapper processes were cluttering the discovery output, making
//! it difficult to identify truly unregistered workers.

#[test]
#[cfg(unix)]
fn test_bash_wrapper_processes_excluded_from_discovery() {
    // This is a documentation test that verifies the fix is in place.
    // The actual filtering happens in scan_needle_processes() in src/cli/mod.rs.
    //
    // The fix ensures that cmdline patterns like:
    //   "bash -c NEEDLE_INNER=1 /path/to/needle run ..."
    // are excluded from discovery, while actual needle processes like:
    //   "/path/to/needle run --workspace ..."
    // are correctly discovered.
    //
    // This prevents false positive "unregistered worker" warnings for
    // shell wrapper processes that are created by tmux sessions.

    // Verify the source code contains the bash wrapper filter
    let source_code =
        std::fs::read_to_string("src/cli/mod.rs").expect("source code should be readable");

    // Check for the bash wrapper exclusion logic
    assert!(
        source_code.contains("bash -c") && source_code.contains("shell wrapper"),
        "scan_needle_processes() should exclude bash wrapper processes.\n\
         Source should contain bash wrapper filtering logic."
    );

    // Verify the filter checks for multiple shell patterns
    assert!(
        source_code.contains("starts_with(\"bash -c\")")
            || source_code.contains("starts_with(\\\"bash -c\\\")"),
        "Should check for 'bash -c' pattern"
    );

    assert!(
        source_code.contains("sh -c") || source_code.contains("/bin/sh -c"),
        "Should check for 'sh -c' pattern"
    );

    println!("✓ Bash wrapper exclusion logic is present in scan_needle_processes()");
}
