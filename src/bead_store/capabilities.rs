//! Plan-transition capability negotiation (plan.md transition N-T11).
//!
//! NEEDLE's authority-changing transitions — attempt receipts, atomic
//! resolution, durable learning facts, and fenced claims — may only run on a
//! backend that explicitly advertises the capability each one needs. The
//! advertisement lives in the same document `bead capabilities --profile
//! native-v1` already returns, under a `transitions` object:
//!
//! ```json
//! {"implementation": "bead-rs", "atomic_claim": true, "...": "...",
//!  "transitions": {"claim_fencing": true}}
//! ```
//!
//! Negotiation is version-agnostic like the rest of the capability contract:
//! a capability is present when its key is advertised as exactly `true`, and
//! absent otherwise — there is no version arithmetic to drift. A backend that
//! omits the `transitions` object advertises none of them, which is the
//! correct answer for every backend that predates this contract.
//!
//! The gate fails closed in both directions:
//!
//! - A transition that is enabled while its capability is absent refuses to
//!   run (see [`BeadRuntimeCapabilities::ensure_transition_support`]); it is
//!   never silently downgraded to the legacy behavior it was meant to replace.
//! - A backend that is not verified for an enabled transition is refused
//!   before it takes authority over bead state (see `crate::bead_store`'s
//!   `open_configured` and the claim-time gate in `crate::claim`).
//!
//! The switches themselves are [`crate::config::TransitionsConfig`], and they
//! default to off: a transition stays disabled until its conformance gate
//! passes, so this module's refusal path is the only thing standing between
//! an operator flipping a flag and an unverified backend mutating beads.
//!
//! Depends on: `types` (nothing), `config` (the flag section only).

use std::collections::BTreeSet;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// A backend capability one plan transition requires before NEEDLE will
/// exercise it.
///
/// The key is the name the backend must advertise under `transitions` in its
/// capability document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TransitionCapability {
    /// The backend records a portable, immutable attempt receipt and can
    /// deduplicate resubmissions of the same attempt (plan.md §7 "Attempt").
    AttemptReceipt,
    /// The backend applies a resolution — event, attempt-tier update, and
    /// lifecycle mutation — as one atomic, auditable transaction
    /// (plan.md §7 "Resolution").
    AtomicResolution,
    /// The backend stores durable, opaque learning-fact references without
    /// interpreting them (plan.md §7 "Reflection").
    DurableFacts,
    /// The backend stamps claims with an epoch/lease and rejects writes from
    /// stale owners (plan.md §7 "Claim"; bead-rs `beadrs-8c343a7c`).
    ClaimFencing,
}

impl TransitionCapability {
    /// The key the backend must advertise to grant this capability.
    pub fn key(self) -> &'static str {
        match self {
            Self::AttemptReceipt => "attempt_receipt",
            Self::AtomicResolution => "atomic_resolution",
            Self::DurableFacts => "durable_facts",
            Self::ClaimFencing => "claim_fencing",
        }
    }

    /// One-line description of what the capability guarantees, for refusal
    /// messages that have to be actionable without reading this source file.
    pub fn guarantees(self) -> &'static str {
        match self {
            Self::AttemptReceipt => "portable, immutable attempt receipts",
            Self::AtomicResolution => "atomic attempt resolution",
            Self::DurableFacts => "durable learning-fact references",
            Self::ClaimFencing => "claim epochs with stale-writer rejection",
        }
    }
}

/// A NEEDLE plan transition whose behavior changes who holds authority over
/// bead state, and which is therefore capability-gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlanTransition {
    /// Attempt identity and receipts (`src/claim`, `src/worker`, `src/prompt`).
    Attempt,
    /// Guarded resolution application (`src/outcome`, `src/resolve`).
    Resolution,
    /// Mature reflection and learning records (`src/learning`, Reflect).
    Learning,
    /// Renewable fenced claim handles (ADR-028).
    FencedClaim,
}

impl PlanTransition {
    /// Every transition, in the canonical order used for reports and gates.
    pub const ALL: [PlanTransition; 4] = [
        PlanTransition::Attempt,
        PlanTransition::Resolution,
        PlanTransition::Learning,
        PlanTransition::FencedClaim,
    ];

    /// The config key (`transitions.<key>.enabled`) that enables this
    /// transition.
    pub fn key(self) -> &'static str {
        match self {
            Self::Attempt => "attempt",
            Self::Resolution => "resolution",
            Self::Learning => "learning",
            Self::FencedClaim => "fenced_claim",
        }
    }

    /// The capabilities that must be advertised before this transition may
    /// run. Every transition requires at least one: a transition with no
    /// requirements would be ungated by construction.
    pub fn required_capabilities(self) -> &'static [TransitionCapability] {
        match self {
            Self::Attempt => &[TransitionCapability::AttemptReceipt],
            Self::Resolution => &[TransitionCapability::AtomicResolution],
            Self::Learning => &[TransitionCapability::DurableFacts],
            Self::FencedClaim => &[TransitionCapability::ClaimFencing],
        }
    }

    /// The transition a config key names, if any.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|transition| transition.key() == key)
    }
}

/// What a backend advertised in one `bead capabilities --profile native-v1`
/// response.
///
/// This is the negotiated snapshot every later check acts on: the store keeps
/// the one captured when it was opened, and the claim-time gate refuses work
/// against anything it cannot account for. Field presence is exact, not
/// inferred — an absent array is an empty array, never a wildcard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadRuntimeCapabilities {
    /// `implementation` from the capability document, or `""` when the
    /// backend sent nothing (identity checks then fail on their own terms).
    pub implementation: String,
    /// Whether the backend asserts atomic claim operations.
    pub atomic_claim: bool,
    /// Lifecycle statuses the backend stores.
    pub statuses: Vec<String>,
    /// Schema URNs (`schema_ref`) the backend speaks.
    pub schemas: Vec<String>,
    /// Command families the backend exposes.
    pub commands: Vec<String>,
    /// Transition capabilities advertised as exactly `true`.
    pub transitions: BTreeSet<String>,
}

impl BeadRuntimeCapabilities {
    /// Whether the backend advertised `capability`.
    pub fn advertises(&self, capability: TransitionCapability) -> bool {
        self.transitions.contains(capability.key())
    }

    /// Every `(transition, capability)` pair required by `transitions` that
    /// this backend did not advertise, in canonical transition order.
    pub fn missing_capabilities(
        &self,
        transitions: &[PlanTransition],
    ) -> Vec<(PlanTransition, TransitionCapability)> {
        let mut missing = Vec::new();
        for transition in PlanTransition::ALL {
            if !transitions.contains(&transition) {
                continue;
            }
            for capability in transition.required_capabilities() {
                if !self.advertises(*capability) {
                    missing.push((transition, *capability));
                }
            }
        }
        missing
    }

    /// Fail closed unless the backend advertises every capability the
    /// enabled `transitions` require.
    ///
    /// The error names each missing pair so an operator can either enable the
    /// capability in the backend or turn the transition back off; it never
    /// falls back to the pre-transition behavior, because the point of the
    /// switch is that the legacy path is what the transition replaces.
    pub fn ensure_transition_support(&self, transitions: &[PlanTransition]) -> Result<()> {
        let missing = self.missing_capabilities(transitions);
        if missing.is_empty() {
            return Ok(());
        }
        let detail = missing
            .iter()
            .map(|(transition, capability)| {
                format!(
                    "transitions.{}.enabled requires {} ({})",
                    transition.key(),
                    capability.key(),
                    capability.guarantees()
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        bail!(
            "backend '{}' does not advertise the capabilities required by the enabled plan transitions and is unsupported for them; failing closed: {detail}",
            if self.implementation.is_empty() {
                "(unidentified)"
            } else {
                &self.implementation
            }
        );
    }
}

/// Parse the capability document a backend answered its probe with.
///
/// Parsing is strict where ambiguity would look like support: a field of the
/// wrong shape is an error rather than an empty default, and a `transitions`
/// entry whose value is not exactly `true` never counts as advertised.
pub fn parse_runtime_capabilities(value: &serde_json::Value) -> Result<BeadRuntimeCapabilities> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("capability document is not a JSON object"))?;

    let implementation = match object.get("implementation") {
        Some(serde_json::Value::String(implementation)) => implementation.clone(),
        Some(_) => bail!("capability document field 'implementation' is not a string"),
        None => String::new(),
    };

    let atomic_claim = match object.get("atomic_claim") {
        Some(serde_json::Value::Bool(atomic_claim)) => *atomic_claim,
        Some(_) => bail!("capability document field 'atomic_claim' is not a boolean"),
        None => false,
    };

    let statuses = string_list(object.get("statuses"), "statuses")?;
    let commands = string_list(object.get("commands"), "commands")?;
    let schemas = schema_refs(object.get("schemas"))?;

    let mut advertised = BTreeSet::new();
    match object.get("transitions") {
        Some(serde_json::Value::Object(entries)) => {
            for (key, value) in entries {
                match value {
                    serde_json::Value::Bool(true) => {
                        advertised.insert(key.clone());
                    }
                    serde_json::Value::Bool(false) => {}
                    _ => bail!(
                        "capability document transitions entry '{key}' is not a boolean; \
                         a capability must be advertised as exactly true or false"
                    ),
                }
            }
        }
        Some(_) => bail!("capability document field 'transitions' is not an object"),
        None => {}
    }

    Ok(BeadRuntimeCapabilities {
        implementation,
        atomic_claim,
        statuses,
        schemas,
        commands,
        transitions: advertised,
    })
}

/// Read a JSON array of strings; absence is an empty list, wrong shapes fail.
fn string_list(value: Option<&serde_json::Value>, field: &str) -> Result<Vec<String>> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(entries)) => {
            let mut parsed = Vec::with_capacity(entries.len());
            for entry in entries {
                match entry.as_str() {
                    Some(text) => parsed.push(text.to_string()),
                    None => bail!("capability document field '{field}' holds a non-string entry"),
                }
            }
            Ok(parsed)
        }
        Some(_) => bail!("capability document field '{field}' is not an array"),
    }
}

/// Read a JSON array of schema references.
///
/// Backends describe each schema as `{"schema_ref": "urn:..."}`; a bare
/// string is accepted as the same statement. Anything else fails closed.
fn schema_refs(value: Option<&serde_json::Value>) -> Result<Vec<String>> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(entries)) => {
            let mut parsed = Vec::with_capacity(entries.len());
            for entry in entries {
                match entry {
                    serde_json::Value::String(schema_ref) => parsed.push(schema_ref.clone()),
                    serde_json::Value::Object(fields) => match fields.get("schema_ref") {
                        Some(serde_json::Value::String(schema_ref)) => {
                            parsed.push(schema_ref.clone())
                        }
                        _ => bail!("capability document schemas entry has no 'schema_ref' string"),
                    },
                    _ => bail!("capability document field 'schemas' holds an unusable entry"),
                }
            }
            Ok(parsed)
        }
        Some(_) => bail!("capability document field 'schemas' is not an array"),
    }
}

/// The transitions `config` enables, in canonical order.
///
/// This is the only place config flags are translated into transition
/// requirements, so a new flag and its gate land in the same expression.
pub fn enabled_plan_transitions(config: &crate::config::TransitionsConfig) -> Vec<PlanTransition> {
    let mut enabled = Vec::new();
    for transition in PlanTransition::ALL {
        let on = match transition {
            PlanTransition::Attempt => config.attempt.enabled,
            PlanTransition::Resolution => config.resolution.enabled,
            PlanTransition::Learning => config.learning.enabled,
            PlanTransition::FencedClaim => config.fenced_claim.enabled,
        };
        if on {
            enabled.push(transition);
        }
    }
    enabled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TransitionsConfig;

    /// A capability document with the base contract satisfied and one
    /// transition capability advertised.
    fn document(transitions: &str) -> serde_json::Value {
        serde_json::json!({
            "implementation": "bead-rs",
            "atomic_claim": true,
            "statuses": ["open", "in_progress", "deferred", "closed"],
            "schemas": [
                {"schema_ref": "urn:bead-rs:schema:issue:native-v1"},
                {"schema_ref": "urn:bead-rs:schema:event:native-v1"},
                {"schema_ref": "urn:bead-rs:schema:field-guide:native-v1"},
            ],
            "commands": ["ref", "data", "query"],
            "transitions": serde_json::from_str::<serde_json::Value>(transitions)
                .expect("transitions fixture is valid JSON"),
        })
    }

    fn parse(transitions: &str) -> BeadRuntimeCapabilities {
        parse_runtime_capabilities(&document(transitions)).expect("fixture parses")
    }

    #[test]
    fn snapshot_is_exactly_what_the_backend_advertised() {
        let snapshot = parse("{\"claim_fencing\": true}");

        assert_eq!(
            snapshot,
            BeadRuntimeCapabilities {
                implementation: "bead-rs".to_string(),
                atomic_claim: true,
                statuses: vec![
                    "open".to_string(),
                    "in_progress".to_string(),
                    "deferred".to_string(),
                    "closed".to_string(),
                ],
                schemas: vec![
                    "urn:bead-rs:schema:issue:native-v1".to_string(),
                    "urn:bead-rs:schema:event:native-v1".to_string(),
                    "urn:bead-rs:schema:field-guide:native-v1".to_string(),
                ],
                commands: vec!["ref".to_string(), "data".to_string(), "query".to_string()],
                transitions: BTreeSet::from(["claim_fencing".to_string()]),
            }
        );
    }

    #[test]
    fn absent_transitions_object_advertises_nothing() {
        let value = serde_json::json!({
            "implementation": "bead-rs",
            "atomic_claim": true,
            "statuses": ["open"],
            "schemas": [{"schema_ref": "urn:bead-rs:schema:issue:native-v1"}],
            "commands": ["query"],
        });

        let snapshot =
            parse_runtime_capabilities(&value).expect("document without transitions parses");
        assert!(snapshot.transitions.is_empty());
        assert_eq!(snapshot.statuses, vec!["open".to_string()]);
        assert_eq!(
            snapshot.schemas,
            vec!["urn:bead-rs:schema:issue:native-v1".to_string()]
        );
    }

    #[test]
    fn capabilities_are_advertised_only_when_exactly_true() {
        let snapshot = parse("{\"claim_fencing\": true, \"atomic_resolution\": false}");

        assert!(snapshot.advertises(TransitionCapability::ClaimFencing));
        assert!(!snapshot.advertises(TransitionCapability::AtomicResolution));
    }

    #[test]
    fn non_boolean_advertisement_is_rejected_rather_than_treated_as_absent() {
        let value = serde_json::json!({
            "implementation": "bead-rs",
            "transitions": {"claim_fencing": "yes"},
        });

        let error = parse_runtime_capabilities(&value).expect_err("string must not advertise");
        assert!(error.to_string().contains("claim_fencing"));
        assert!(error.to_string().contains("exactly true"));
    }

    #[test]
    fn non_boolean_base_fields_are_rejected_rather_than_defaulted() {
        let value = serde_json::json!({"implementation": "bead-rs", "atomic_claim": "true"});
        let error = parse_runtime_capabilities(&value).expect_err("string atomic_claim");
        assert!(error
            .to_string()
            .contains("'atomic_claim' is not a boolean"));

        let value = serde_json::json!({"implementation": 7});
        let error = parse_runtime_capabilities(&value).expect_err("numeric implementation");
        assert!(error
            .to_string()
            .contains("'implementation' is not a string"));
    }

    #[test]
    fn non_object_capability_document_is_rejected() {
        let error = parse_runtime_capabilities(&serde_json::json!(["bead-rs"]))
            .expect_err("array is not a document");
        assert!(error.to_string().contains("not a JSON object"));
    }

    #[test]
    fn schema_refs_normalize_object_and_bare_string_forms() {
        let value = serde_json::json!({
            "implementation": "bead-rs",
            "schemas": [
                {"schema_ref": "urn:bead-rs:schema:issue:native-v1"},
                "urn:bead-rs:schema:event:native-v1",
            ],
        });

        let snapshot = parse_runtime_capabilities(&value).expect("both forms parse");
        assert_eq!(
            snapshot.schemas,
            vec![
                "urn:bead-rs:schema:issue:native-v1".to_string(),
                "urn:bead-rs:schema:event:native-v1".to_string(),
            ]
        );
    }

    #[test]
    fn schema_entry_without_schema_ref_is_rejected() {
        let value =
            serde_json::json!({"implementation": "bead-rs", "schemas": [{"name": "issue"}]});
        let error = parse_runtime_capabilities(&value).expect_err("entry without schema_ref");
        assert!(error.to_string().contains("schema_ref"));
    }

    #[test]
    fn missing_capabilities_report_transition_and_capability_pairs() {
        // Every backend in this repository today: no transitions advertised.
        let snapshot = parse("{}");

        let missing = snapshot.missing_capabilities(&PlanTransition::ALL);
        assert_eq!(
            missing,
            vec![
                (
                    PlanTransition::Attempt,
                    TransitionCapability::AttemptReceipt
                ),
                (
                    PlanTransition::Resolution,
                    TransitionCapability::AtomicResolution
                ),
                (PlanTransition::Learning, TransitionCapability::DurableFacts),
                (
                    PlanTransition::FencedClaim,
                    TransitionCapability::ClaimFencing
                ),
            ]
        );
    }

    #[test]
    fn missing_capabilities_skip_transitions_that_are_not_enabled() {
        let snapshot = parse("{}");

        let missing = snapshot.missing_capabilities(&[PlanTransition::FencedClaim]);
        assert_eq!(
            missing,
            vec![(
                PlanTransition::FencedClaim,
                TransitionCapability::ClaimFencing
            )]
        );
    }

    #[test]
    fn transition_support_fails_closed_naming_each_requirement() {
        let snapshot = parse("{\"claim_fencing\": true}");

        let error = snapshot
            .ensure_transition_support(&[PlanTransition::FencedClaim, PlanTransition::Learning])
            .expect_err("durable_facts is not advertised");
        let message = error.to_string();
        assert!(message.contains("failing closed"), "unexpected: {message}");
        assert!(message.contains("transitions.learning.enabled"),);
        assert!(message.contains("durable_facts"));
        assert!(message.contains("durable learning-fact references"));
        // The advertised capability must not be listed as missing.
        assert!(!message.contains("fenced_claim"), "unexpected: {message}");
    }

    #[test]
    fn transition_support_succeeds_when_every_requirement_is_advertised() {
        let snapshot = parse(
            "{\"attempt_receipt\": true, \"atomic_resolution\": true, \
              \"durable_facts\": true, \"claim_fencing\": true}",
        );

        snapshot
            .ensure_transition_support(&PlanTransition::ALL)
            .expect("all four transitions are supported");
    }

    /// Compatibility fallback: with no transition enabled, a backend that
    /// advertises nothing at all is still fully supported.
    #[test]
    fn no_enabled_transition_never_requires_a_capability() {
        let snapshot = parse("{}");

        snapshot
            .ensure_transition_support(&[])
            .expect("legacy behavior needs nothing from the backend");
    }

    #[test]
    fn every_transition_requires_at_least_one_capability_and_unique_key() {
        let mut keys = Vec::new();
        for transition in PlanTransition::ALL {
            assert!(
                !transition.required_capabilities().is_empty(),
                "{} gates on nothing",
                transition.key()
            );
            assert!(
                PlanTransition::from_key(transition.key()) == Some(transition),
                "{} does not round-trip through from_key",
                transition.key()
            );
            keys.push(transition.key());
        }
        assert_eq!(
            keys.iter().collect::<std::collections::HashSet<_>>().len(),
            keys.len(),
            "transition keys are not unique: {keys:?}"
        );
        assert!(PlanTransition::from_key("nope").is_none());
    }

    #[test]
    fn enabled_plan_transitions_follows_the_config_in_canonical_order() {
        let mut config = TransitionsConfig::default();
        assert!(enabled_plan_transitions(&config).is_empty());

        config.fenced_claim.enabled = true;
        config.attempt.enabled = true;
        assert_eq!(
            enabled_plan_transitions(&config),
            vec![PlanTransition::Attempt, PlanTransition::FencedClaim]
        );

        config.resolution.enabled = true;
        config.learning.enabled = true;
        assert_eq!(
            enabled_plan_transitions(&config),
            PlanTransition::ALL.to_vec()
        );
    }
}
