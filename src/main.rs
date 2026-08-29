//! NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort.

use anyhow::Result;
use std::panic::{self, PanicHookInfo};
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
use libc::{signal, SIGPIPE, SIG_DFL};

/// Global flag to track if we're exiting due to BrokenPipe
static IS_BROKEN_PIPE: AtomicBool = AtomicBool::new(false);

/// Custom panic hook that suppresses BrokenPipe panics.
///
/// When stdout is closed (e.g., `needle status | head`), Rust's std library
/// catches the EPIPE error and panics with "Broken pipe". This hook exits
/// cleanly instead of showing a panic traceback.
fn panic_hook(info: &PanicHookInfo) {
    // Check if this is a BrokenPipe panic
    let panic_msg = info.to_string();
    if panic_msg.contains("Broken pipe") || panic_msg.contains("failed printing to stdout") {
        // Set the flag so we can detect this in other threads
        IS_BROKEN_PIPE.store(true, Ordering::SeqCst);
        // Exit silently - the pipe reader already got what it needed
        std::process::exit(0);
    }

    // For other panics, print to stderr and exit with error
    if !IS_BROKEN_PIPE.load(Ordering::SeqCst) {
        eprintln!("Panic: {}", info);
        std::process::exit(101);
    }

    std::process::exit(0);
}

fn main() -> Result<()> {
    // Install custom panic hook BEFORE any other initialization
    // This ensures we catch BrokenPipe panics from any thread
    let hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // Check if this is a BrokenPipe panic first
        let panic_msg = info.to_string();
        if panic_msg.contains("Broken pipe") || panic_msg.contains("failed printing to stdout") {
            // Exit silently - the pipe reader already got what it needed
            std::process::exit(0);
        }
        // For other panics, use the default hook
        hook(info);
    }));

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
