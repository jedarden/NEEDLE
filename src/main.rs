//! NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort.

use anyhow::Result;
use std::panic;

#[cfg(unix)]
use libc::{signal, SIGPIPE, SIG_DFL};

fn main() -> Result<()> {
    // ADR-030 decision 5 (N-T52): a spawned test harness must carry an
    // isolated state directory. Refuse before anything can write.
    if let Err(message) = needle::state_dir::ensure_harness_isolation() {
        eprintln!("Error: {message}");
        std::process::exit(1);
    }

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
    match needle::cli::run() {
        Ok(()) => Ok(()),
        Err(e) => {
            // N-T33: a one-shot caller refused by launch admission is not a
            // work failure — nothing was claimed — so it exits with a
            // distinct temporary-unavailable code instead of the generic
            // failure exit. Service-managed runs hold resident and never
            // produce this error.
            if e.downcast_ref::<needle::types::AdmissionUnavailable>()
                .is_some()
            {
                eprintln!("Error: {e:#}");
                std::process::exit(needle::types::EXIT_ADMISSION_UNAVAILABLE);
            }
            Err(e)
        }
    }
}
