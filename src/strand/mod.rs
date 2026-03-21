//! Strand waterfall: ordered sequence of selection strategies.
//!
//! The StrandRunner evaluates strands in priority order. The first strand
//! that yields a candidate wins. Strands are stateless — they receive queue
//! state and return a candidate or nothing.
//!
//! Depends on: `types`, `config`, `bead_store`.

use anyhow::Result;

use crate::bead_store::BeadStore;
use crate::config::Config;
use crate::types::{BeadId, StrandResult};

/// A single selection strategy in the waterfall.
#[async_trait::async_trait]
pub trait Strand: Send + Sync {
    /// Human-readable name for telemetry.
    fn name(&self) -> &str;

    /// Evaluate this strand against the current queue state.
    async fn evaluate(&self, store: &dyn BeadStore) -> StrandResult;
}

/// Runs strands in order, returning the first candidate found.
pub struct StrandRunner {
    strands: Vec<Box<dyn Strand>>,
}

impl StrandRunner {
    pub fn new(strands: Vec<Box<dyn Strand>>) -> Self {
        StrandRunner { strands }
    }

    /// Build the default strand waterfall from config.
    pub fn from_config(_config: &Config) -> Self {
        // TODO(needle-sxp): construct default strands (Pluck → Mend → Defer)
        StrandRunner { strands: vec![] }
    }

    /// Run the waterfall, returning the first candidate bead ID or None.
    pub async fn select(&self, store: &dyn BeadStore) -> Result<Option<BeadId>> {
        for strand in &self.strands {
            match strand.evaluate(store).await {
                StrandResult::BeadFound(beads) => {
                    if let Some(bead) = beads.into_iter().next() {
                        return Ok(Some(bead.id));
                    }
                    continue;
                }
                StrandResult::WorkCreated => {
                    // New work was synthesized; restart the waterfall from scratch.
                    return Ok(None);
                }
                StrandResult::NoWork => continue,
                StrandResult::Error(e) => {
                    tracing::warn!(
                        strand = strand.name(),
                        error = %e,
                        "strand error, continuing to next strand"
                    );
                    continue;
                }
            }
        }
        Ok(None)
    }
}
