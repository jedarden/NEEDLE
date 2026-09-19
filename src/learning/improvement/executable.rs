//! Executability admission (N-T07, `needle-8c3520f7`).
//!
//! A proposal can be perfectly well-evidenced and still be unworkable. "Improve
//! coverage" names a real desire, cites real evidence, and cannot be finished:
//! there is no state of the repository in which it is done. Admitting one
//! produces a bead that a worker picks up, does something plausible to, and
//! closes — which is the false-progress shape the plan spends section 10
//! refusing.
//!
//! So a proposal becomes work only when someone could tell, mechanically, that
//! it is finished. [`assess`] requires all of:
//!
//! - **One machine-checkable acceptance command.** Exactly one: a chain is two
//!   claims about doneness and admits the second passing while the first was
//!   never run.
//! - **A bounded overlap scope.** The paths the change would touch, so two
//!   proposals over the same file can be serialized instead of racing (the
//!   duplicate-claim failure of ADR-015).
//! - **Immutable evidence, or an approved intent.** Normally the proposal's own
//!   evidence refs; an operator-approved intent is the escape hatch for work
//!   whose justification is a decision rather than an observation.
//! - **A declared effort estimate and dependencies.**
//! - **A stable deduplication fingerprint** — the envelope's derived signature.
//!
//! Everything here is a pure check over records.

use serde::{Deserialize, Serialize};

use super::envelope::ImprovementProposal;
use super::impact::ImpactContract;

/// Command prefixes the fleet can actually run as an acceptance check.
///
/// An acceptance command is a promise that a machine can decide doneness, so
/// the vocabulary is an allowlist rather than "anything that looks like a
/// shell command": an arbitrary string here would let a proposal declare
/// `echo done` and be structurally admissible forever.
const MACHINE_CHECKABLE_PREFIXES: &[&str] = &[
    "cargo test",
    "cargo clippy",
    "cargo check",
    "cargo fmt",
    "bash tests/",
    "bash scripts/",
    "./scripts/",
    "scripts/",
];

/// Shell metacharacters that would make one string into several commands.
const COMMAND_CHAINERS: &[&str] = &["&&", "||", ";", "|", "\n", "`", "$("];

/// Phrases that describe a direction rather than a destination.
///
/// Matching one is not itself fatal — "reduce the timeout to 600s in
/// `src/x.rs`" is concrete despite starting with a direction word. It is fatal
/// only when the request also fails to name anything concrete, which is what
/// [`names_something_concrete`] decides.
const DIRECTIONAL_PHRASES: &[&str] = &[
    "improve",
    "better",
    "optimize",
    "optimise",
    "clean up",
    "cleanup",
    "tidy",
    "refactor",
    "enhance",
    "modernize",
    "modernise",
    "more robust",
    "as needed",
    "where appropriate",
];

/// What a proposal must declare before it can become work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPlan {
    /// The single command that decides whether the work is done.
    pub acceptance_command: String,
    /// Paths or modules the change would touch.
    pub overlap_scope: Vec<String>,
    /// Beads or proposals that must land first.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// An operator-approved intent, for work justified by a decision rather
    /// than an observation. Evidence is the normal path; this is the
    /// exception, and it is operator-owned.
    #[serde(default)]
    pub approved_intent: Option<String>,
}

/// Why a proposal is not executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NonExecutable {
    /// No acceptance command at all.
    NoAcceptanceCommand,
    /// More than one command, or a shell chain pretending to be one.
    NotASingleCommand {
        /// The offending metacharacter.
        chainer: String,
    },
    /// The command is not something the fleet can run and check.
    NotMachineCheckable {
        /// The command as supplied.
        command: String,
    },
    /// The change declares no paths, so nothing bounds it.
    UnboundedScope,
    /// The request states a direction, not a destination.
    NotConcrete {
        /// The directional phrase that was matched.
        phrase: String,
    },
    /// Neither evidence nor an approved intent.
    NoEvidenceOrApprovedIntent,
    /// The deduplication fingerprint is missing.
    NoStableFingerprint,
}

impl std::fmt::Display for NonExecutable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NonExecutable::NoAcceptanceCommand => write!(
                f,
                "no acceptance command: nothing could decide that this is done"
            ),
            NonExecutable::NotASingleCommand { chainer } => write!(
                f,
                "acceptance command chains on {chainer:?}: one proposal declares one \
                 check, or a later stage can pass while an earlier one never ran"
            ),
            NonExecutable::NotMachineCheckable { command } => write!(
                f,
                "acceptance command {command:?} is not one the fleet can run and check; \
                 expected one of: {}",
                MACHINE_CHECKABLE_PREFIXES.join(", ")
            ),
            NonExecutable::UnboundedScope => write!(
                f,
                "no overlap scope: a change that names no paths cannot be bounded, \
                 and two such changes cannot be serialized against each other"
            ),
            NonExecutable::NotConcrete { phrase } => write!(
                f,
                "request is directional ({phrase:?}) and names nothing concrete — \
                 no file, symbol, command or identifier — so no repository state \
                 would finish it"
            ),
            NonExecutable::NoEvidenceOrApprovedIntent => write!(
                f,
                "neither immutable evidence nor an operator-approved intent"
            ),
            NonExecutable::NoStableFingerprint => {
                write!(f, "no stable deduplication fingerprint")
            }
        }
    }
}

/// A proposal that passed every executability check.
///
/// Carrying the plan alongside the proposal means downstream stages cannot
/// reach a proposal's plan without having gone through [`assess`].
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutableProposal {
    /// The proposal itself.
    pub proposal: ImprovementProposal,
    /// Its execution plan.
    pub plan: ExecutionPlan,
}

/// Decide whether a proposal could actually be finished.
pub fn assess(
    proposal: &ImprovementProposal,
    contract: &ImpactContract,
    plan: &ExecutionPlan,
) -> Result<ExecutableProposal, NonExecutable> {
    if proposal.signature.is_empty() {
        return Err(NonExecutable::NoStableFingerprint);
    }

    // Evidence normally comes from the envelope, which already refuses an
    // empty list; the approved-intent branch is the operator's exception for
    // work justified by a decision.
    let has_intent = plan
        .approved_intent
        .as_ref()
        .is_some_and(|intent| !intent.trim().is_empty());
    if proposal.evidence.is_empty() && !has_intent {
        return Err(NonExecutable::NoEvidenceOrApprovedIntent);
    }

    let command = plan.acceptance_command.trim();
    if command.is_empty() {
        return Err(NonExecutable::NoAcceptanceCommand);
    }
    if let Some(chainer) = COMMAND_CHAINERS
        .iter()
        .find(|chainer| command.contains(**chainer))
    {
        return Err(NonExecutable::NotASingleCommand {
            chainer: (*chainer).to_string(),
        });
    }
    if !MACHINE_CHECKABLE_PREFIXES
        .iter()
        .any(|prefix| command.starts_with(prefix))
    {
        return Err(NonExecutable::NotMachineCheckable {
            command: command.to_string(),
        });
    }

    let scoped: Vec<&String> = plan
        .overlap_scope
        .iter()
        .filter(|entry| !entry.trim().is_empty())
        .collect();
    if scoped.is_empty() {
        return Err(NonExecutable::UnboundedScope);
    }

    // Directional language is only a problem when nothing concrete accompanies
    // it. The overlap scope counts as concrete, which is why a well-formed
    // "reduce X in src/y.rs" passes and a bare "improve coverage" does not.
    if !names_something_concrete(&proposal.intended_change, &scoped) {
        if let Some(phrase) = directional_phrase(&proposal.intended_change) {
            return Err(NonExecutable::NotConcrete { phrase });
        }
    }

    // The effort estimate is carried by the contract's producer evidence and
    // is non-optional there, so reaching this point means it was declared.
    let _ = contract.producer.effort;

    Ok(ExecutableProposal {
        proposal: proposal.clone(),
        plan: plan.clone(),
    })
}

/// The first directional phrase in `text`, if any.
fn directional_phrase(text: &str) -> Option<String> {
    let lowered = text.to_lowercase();
    DIRECTIONAL_PHRASES
        .iter()
        .find(|phrase| lowered.contains(**phrase))
        .map(|phrase| (*phrase).to_string())
}

/// Whether the request points at something a reader could go and look at.
///
/// A path, a Rust module path, a file extension, a backticked identifier, or a
/// declared overlap scope that itself looks like a path. The scope is the
/// strongest signal, which is why a concrete uncovered branch plus a focused
/// test command is admissible however the prose is worded.
fn names_something_concrete(intended_change: &str, scope: &[&String]) -> bool {
    let scope_is_concrete = scope.iter().any(|entry| looks_like_a_path(entry));
    if scope_is_concrete {
        return true;
    }
    intended_change.contains('`')
        || intended_change.contains("::")
        || looks_like_a_path(intended_change)
}

/// Whether a string points at a file or module.
fn looks_like_a_path(text: &str) -> bool {
    text.contains('/') || text.contains(".rs") || text.contains(".toml") || text.contains(".yaml")
}
