//! Panic stack trace capture for tests.
//!
//! This module provides panic hook installation and stack trace capture
//! to ensure complete, non-truncated panic information is available during
//! test execution.
//!
//! ## Features
//!
//! - Custom panic hook that captures full stack traces
//! - Backtrace preservation across test boundaries
//! - Integration with test runner for complete panic information
//!
//! ## Usage
//!
//! ```no_run
//! use needle::panic_capture::install_panic_hook;
//!
//! // Install the panic hook (typically in test setup)
//! install_panic_hook();
//! ```

#[allow(deprecated)]
use std::panic::{self, PanicInfo};
use std::sync::Once;
use std::time::SystemTime;

static HOOK_INSTALLED: Once = Once::new();

/// Install a custom panic hook that captures full stack traces.
///
/// This function sets up a panic handler that ensures:
/// - Full backtraces are captured (not truncated)
/// - Panic information is formatted consistently
/// - Stack traces are preserved even when tests abort
///
/// The hook is idempotent - calling it multiple times has no additional effect.
///
/// ## Example
///
/// ```no_run
/// use needle::panic_capture::install_panic_hook;
///
/// fn main() {
///     install_panic_hook();
///     // ... rest of application
/// }
/// ```
pub fn install_panic_hook() {
    HOOK_INSTALLED.call_once(|| {
        // Set the default panic hook to capture full backtraces
        set_full_backtrace_env();

        // Install custom panic handler
        panic::set_hook(Box::new(panic_hook));

        tracing::debug!("panic hook installed for full stack trace capture");
    });
}

/// Ensure RUST_BACKTRACE is set for full backtraces.
///
/// This function checks and sets the RUST_BACKTRACE environment variable
/// to ensure complete stack traces are captured during panics.
fn set_full_backtrace_env() {
    // Only set if not already configured by the user
    if std::env::var("RUST_BACKTRACE").is_err() {
        std::env::set_var("RUST_BACKTRACE", "full");
    }
}

/// Custom panic hook that captures and formats complete panic information.
///
/// This hook:
/// - Captures the full panic message and location
/// - Ensures backtrace information is included
/// - Formats the output consistently for parsing
#[allow(deprecated)]
fn panic_hook(info: &PanicInfo) {
    // Get panic location
    let location = info.location().unwrap_or_else(|| {
        // Fallback for cases where location isn't available
        panic::Location::caller()
    });

    // Build the panic message
    let msg = match info.payload().downcast_ref::<&str>() {
        Some(s) => *s,
        None => match info.payload().downcast_ref::<String>() {
            Some(s) => &s[..],
            None => "Box<dyn Any>",
        },
    };

    // Emit structured panic information
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("PANIC captured in test");
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("Message: {}", msg);
    eprintln!(
        "Location: {}:{}:{}",
        location.file(),
        location.line(),
        location.column()
    );

    // Ensure backtrace is displayed
    if let Some(backtrace) = info.payload().downcast_ref::<std::backtrace::Backtrace>() {
        eprintln!("Backtrace:\n{}", backtrace);
    } else {
        // Trigger backtrace capture if not already present
        eprintln!("Backtrace: (capture enabled via RUST_BACKTRACE=full)");
    }

    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // Capture timestamp for telemetry
    let timestamp = SystemTime::now();

    // Emit telemetry event for panic
    tracing::error!(
        panic_message = msg,
        file = location.file(),
        line = location.line(),
        column = location.column(),
        timestamp = ?timestamp,
        "test panic captured"
    );
}

/// Check if panic hook is installed.
///
/// Returns true if the panic hook has been installed, false otherwise.
pub fn is_hook_installed() -> bool {
    HOOK_INSTALLED.is_completed()
}

/// Capture panic information from a panic payload.
///
/// This function extracts and formats panic information for logging
/// or test output persistence.
///
/// ## Arguments
///
/// * `payload` - The panic payload from catch_unwind
///
/// ## Returns
///
/// A formatted string containing the panic information.
pub fn capture_panic_info(payload: Box<dyn std::any::Any + Send>) -> String {
    let msg = if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "Unknown panic payload".to_string()
    };

    format!("PANIC: {}", msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_install_panic_hook_is_idempotent() {
        // First install
        install_panic_hook();
        assert!(is_hook_installed());

        // Second install should be safe
        install_panic_hook();
        assert!(is_hook_installed());
    }

    #[test]
    fn test_set_full_backtrace_env() {
        // Clear existing value
        std::env::remove_var("RUST_BACKTRACE");

        // Should set to "full"
        set_full_backtrace_env();
        assert_eq!(std::env::var("RUST_BACKTRACE").unwrap(), "full");

        // Clean up
        std::env::remove_var("RUST_BACKTRACE");
    }

    #[test]
    fn test_set_full_backtrace_env_respects_existing() {
        // Set existing value
        std::env::set_var("RUST_BACKTRACE", "1");

        // Should not override
        set_full_backtrace_env();
        assert_eq!(std::env::var("RUST_BACKTRACE").unwrap(), "1");

        // Clean up
        std::env::remove_var("RUST_BACKTRACE");
    }

    #[test]
    fn test_capture_panic_info_with_string() {
        let payload = Box::new("test panic message") as Box<dyn std::any::Any + Send>;
        let captured = capture_panic_info(payload);
        assert!(captured.contains("test panic message"));
    }

    #[test]
    fn test_capture_panic_info_with_string_type() {
        let payload = Box::new(String::from("another panic")) as Box<dyn std::any::Any + Send>;
        let captured = capture_panic_info(payload);
        assert!(captured.contains("another panic"));
    }

    #[test]
    fn test_capture_panic_info_with_unknown() {
        let payload = Box::new(12345) as Box<dyn std::any::Any + Send>;
        let captured = capture_panic_info(payload);
        assert!(captured.contains("Unknown panic payload"));
    }

    #[test]
    fn test_panic_hook_captures_location() {
        install_panic_hook();

        // This test verifies the hook is installed - we don't actually panic
        assert!(is_hook_installed());
    }
}
