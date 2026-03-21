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
use crate::types::{Bead, BeadId};

/// Result of a strand evaluation.
#[derive(Debug)]
pub enum StrandResult {
    /// A candidate bead was found.
    Candidate(Bead),
    /// This strand found nothing; continue to next strand.
    Empty,
    /// The strand encountered an error during evaluation.
    Error(anyhow::Error),
}

/// A single selection strategy.
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
        // TODO(needle-nva): construct default strands (Mend → Explore → Defer)
        StrandRunner { strands: vec![] }
    }

    /// Run the waterfall, returning the first candidate or None.
    pub async fn select(&self, store: &dyn BeadStore) -> Result<Option<BeadId>> {
        for strand in &self.strands {
            match strand.evaluate(store).await {
                StrandResult::Candidate(bead) => return Ok(Some(bead.id)),
                StrandResult::Empty => continue,
                StrandResult::Error(e) => {
                    tracing::warn!(strand = strand.name(), error = %e, "strand error, continuing");
                    continue;
                }
            }
        }
        Ok(None)
    }
}
