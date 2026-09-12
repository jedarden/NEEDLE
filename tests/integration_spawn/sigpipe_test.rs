//! SIGPIPE handling tests
//!
//! These tests verify that NEEDLE exits cleanly when stdout is closed
//! by a pipe reader (e.g., `needle status | head`), rather than panicking
//! with "Broken pipe" and exit code 101.

use std::io::Read;
use std::process::{Command, Stdio};

#[test]
fn test_sigpipe_on_closed_stdout() {
    // Get the needle binary path
    let bin_path = std::env::var("CARGO_BIN_EXE_needle").unwrap_or_else(|_| "needle".to_string());

    // Create a pipe for stdout
    let (mut reader, writer) = os_pipe::pipe().expect("failed to create pipe");

    // Spawn needle with stdout piped
    let mut child = Command::new(&bin_path)
        .arg("config")
        .stdout(Stdio::from(writer))
        .spawn()
        .expect("failed to spawn needle");

    // Read just one byte from the pipe, then close the reader
    // This simulates `head -1` exiting after reading one line
    let mut buffer = [0u8; 1];
    let _ = reader.read_exact(&mut buffer);
    drop(reader); // Close the reader end

    // Wait for the child to exit
    let status = child.wait().expect("failed to wait for child");

    // On Unix systems with SIG_DFL, the exit status should be:
    // - 0 if the process handled the broken pipe gracefully
    // - 141 (128 + 13) if killed by SIGPIPE
    //
    // It should NOT be 101 (Rust's panic exit code)
    #[cfg(unix)]
    {
        let exit_code = status.code().unwrap_or(0);
        assert_ne!(
            exit_code,
            101,
            "needle exited with panic code 101 on broken pipe; stderr:\n{:?}",
            String::from_utf8_lossy(&[])
        );
        assert!(
            exit_code == 0 || exit_code == 141,
            "unexpected exit code {} on broken pipe (expected 0 or 141)",
            exit_code
        );
    }

    #[cfg(not(unix))]
    {
        // On non-Unix systems, we just verify it didn't panic
        let exit_code = status.code().unwrap_or(0);
        assert_ne!(
            exit_code, 101,
            "needle exited with panic code 101 on broken pipe"
        );
    }
}
