//! NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort.

use anyhow::Result;

#[cfg(unix)]
use libc::{signal, SIGPIPE, SIG_DFL};

fn main() -> Result<()> {
    // Restore default SIGPIPE disposition on Unix systems.
    // This ensures the process exits cleanly (exit code 141) when stdout
    // is closed by a pipe reader (e.g., `needle status | head`), rather
    // than panicking with "Broken pipe" and exit code 101.
    #[cfg(unix)]
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }

    // Don't set a global tracing subscriber here.
    // The CLI layer will initialize it with OTel support after loading config.
    needle::cli::run()
}
