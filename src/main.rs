//! NEEDLE — Navigates Every Enqueued Deliverable, Logs Effort.

use anyhow::Result;

fn main() -> Result<()> {
    // Don't set a global tracing subscriber here.
    // The CLI layer will initialize it with OTel support after loading config.
    needle::cli::run()
}
