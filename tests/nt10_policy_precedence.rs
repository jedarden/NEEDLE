use std::path::Path;

use needle::policy::{
    Authority, AuthorityRegistry, PolicyError, PolicyKind, PolicyScope, PolicySource,
    ResolutionContext,
};

fn source(id: &str, kind: PolicyKind, scope: PolicyScope, content: &str) -> PolicySource {
    PolicySource::new(id, kind, scope, content).expect("fixture source is valid")
}

#[test]
fn precedence_is_derived_from_the_typed_registry_and_scope_is_deterministic() {
    let fixture = tempfile::tempdir().expect("isolated fixture directory");
    let repository = fixture.path().join("repository");
    let nested = repository.join("services/api");
    let target = nested.join("src/lib.rs");

    let mut registry = AuthorityRegistry::new();
    registry
        .insert(source(
            "memory-hint",
            PolicyKind::AdvisoryMemory,
            PolicyScope::Global,
            "prefer a concise patch",
        ))
        .unwrap();
    registry
        .insert(source(
            "current-plan",
            PolicyKind::CurrentPlan,
            PolicyScope::Global,
            "implement the acceptance contract",
        ))
        .unwrap();
    registry
        .insert(source(
            "accepted-adr",
            PolicyKind::AcceptedAdr,
            PolicyScope::Global,
            "policy conflicts are visible",
        ))
        .unwrap();
    registry
        .insert(source(
            "gate-config",
            PolicyKind::ExecutableGate,
            PolicyScope::repository(&repository),
            "run the fast definition of done",
        ))
        .unwrap();
    registry
        .insert(source(
            "codex-projection",
            PolicyKind::AdapterInstructions,
            PolicyScope::adapter("codex"),
            "use the explicit context bundle",
        ))
        .unwrap();
    registry
        .insert(source(
            "agents-root",
            PolicyKind::RepositoryInstructions,
            PolicyScope::repository(&repository),
            "preserve shared checkout changes",
        ))
        .unwrap();
    registry
        .insert(source(
            "agents-nested",
            PolicyKind::RepositoryInstructions,
            PolicyScope::directory(&nested),
            "use the service-local instructions",
        ))
        .unwrap();
    registry
        .insert(source(
            "external-safety",
            PolicyKind::ExternalConstraint,
            PolicyScope::Global,
            "do not expose credentials",
        ))
        .unwrap();

    let resolved = registry
        .resolve(&ResolutionContext::new(&target).for_adapter("codex"))
        .expect("all fixture sources are unambiguous");

    assert_eq!(
        resolved.source_ids(),
        vec![
            "external-safety",
            "agents-nested",
            "agents-root",
            "codex-projection",
            "gate-config",
            "accepted-adr",
            "current-plan",
            "memory-hint",
        ]
    );
    assert_eq!(
        resolved.winning_source().unwrap().id.as_str(),
        "external-safety"
    );
    assert_eq!(
        PolicyKind::ExternalConstraint.authority(),
        Authority::Safety
    );
    assert_eq!(PolicyKind::AdvisoryMemory.authority(), Authority::Memory);
}

#[test]
fn resolution_excludes_sources_outside_the_target_or_adapter_scope() {
    let fixture = tempfile::tempdir().expect("isolated fixture directory");
    let repository = fixture.path().join("repository");
    let other_repository = fixture.path().join("other-repository");
    let target = repository.join("src/lib.rs");

    let mut registry = AuthorityRegistry::new();
    registry
        .insert(source(
            "global",
            PolicyKind::CurrentPlan,
            PolicyScope::Global,
            "global plan",
        ))
        .unwrap();
    registry
        .insert(source(
            "other-repository",
            PolicyKind::ExternalConstraint,
            PolicyScope::repository(&other_repository),
            "must not apply",
        ))
        .unwrap();
    registry
        .insert(source(
            "wrong-adapter",
            PolicyKind::AdapterInstructions,
            PolicyScope::adapter("claude"),
            "must not apply",
        ))
        .unwrap();

    let resolved = registry
        .resolve(&ResolutionContext::new(&target).for_adapter("codex"))
        .unwrap();

    assert_eq!(resolved.source_ids(), vec!["global"]);
}

#[test]
fn equal_authority_and_scope_conflicts_fail_with_stable_source_order() {
    let fixture = tempfile::tempdir().expect("isolated fixture directory");
    let repository = fixture.path().join("repository");
    let target = repository.join("src/lib.rs");

    let build_registry = |first: &str, second: &str| {
        let mut registry = AuthorityRegistry::new();
        registry
            .insert(source(
                first,
                PolicyKind::RepositoryInstructions,
                PolicyScope::repository(&repository),
                "run the required checks",
            ))
            .unwrap();
        registry
            .insert(source(
                second,
                PolicyKind::RepositoryInstructions,
                PolicyScope::repository(&repository),
                "skip the required checks",
            ))
            .unwrap();
        registry
    };

    let context = ResolutionContext::new(Path::new(&target));
    let first = build_registry("z-source", "a-source")
        .resolve(&context)
        .expect_err("same authority and scope must not be silently merged");
    let second = build_registry("a-source", "z-source")
        .resolve(&context)
        .expect_err("insertion order must not affect the failure");

    assert_eq!(first, second);
    assert_eq!(
        first,
        PolicyError::Conflict {
            authority: Authority::Repository,
            scope: PolicyScope::repository(&repository),
            sources: vec![
                needle::policy::PolicySourceId::new("a-source").unwrap(),
                needle::policy::PolicySourceId::new("z-source").unwrap(),
            ],
        }
    );
}
