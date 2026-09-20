//! Execution must fail closed when its required policy authority is absent or
//! ambiguous (N-T10, `needle-91f5e616`).

use std::path::Path;

use needle::policy::{
    Authority, AuthorityRegistry, ExecutionPolicyRequirements, PolicyAdmissionError, PolicyKind,
    PolicyScope, PolicySource, ResolutionContext,
};

fn source(id: &str, kind: PolicyKind, scope: PolicyScope, content: &str) -> PolicySource {
    PolicySource::new(id, kind, scope, content).expect("fixture source is valid")
}

fn insert(registry: &mut AuthorityRegistry, source: PolicySource) {
    registry
        .insert(source)
        .expect("fixture source IDs are unique");
}

fn context(repository: &Path) -> ResolutionContext {
    ResolutionContext::new(repository.join("src/lib.rs")).for_adapter("codex")
}

fn complete_registry(repository: &Path) -> AuthorityRegistry {
    let mut registry = AuthorityRegistry::new();
    insert(
        &mut registry,
        source(
            "external-safety",
            PolicyKind::ExternalConstraint,
            PolicyScope::Global,
            "do not expose credentials",
        ),
    );
    insert(
        &mut registry,
        source(
            "repository-instructions",
            PolicyKind::RepositoryInstructions,
            PolicyScope::repository(repository),
            "preserve repository policy",
        ),
    );
    insert(
        &mut registry,
        source(
            "adapter-instructions",
            PolicyKind::AdapterInstructions,
            PolicyScope::adapter("codex"),
            "use the explicit context bundle",
        ),
    );
    insert(
        &mut registry,
        source(
            "execution-gate",
            PolicyKind::ExecutableGate,
            PolicyScope::repository(repository),
            "run the required checks",
        ),
    );
    registry
}

#[test]
fn missing_required_authority_rejects_execution() {
    let fixture = tempfile::tempdir().expect("isolated policy fixture");
    let repository = fixture.path().join("repository");

    // Repository policy is intentionally removed by constructing a registry
    // that contains the other execution authorities only.  The registry is
    // the complete execution input, so no ambient HOME or checkout policy can
    // satisfy this requirement.
    let mut registry = AuthorityRegistry::new();
    insert(
        &mut registry,
        source(
            "external-safety",
            PolicyKind::ExternalConstraint,
            PolicyScope::Global,
            "do not expose credentials",
        ),
    );
    insert(
        &mut registry,
        source(
            "adapter-instructions",
            PolicyKind::AdapterInstructions,
            PolicyScope::adapter("codex"),
            "use the explicit context bundle",
        ),
    );
    insert(
        &mut registry,
        source(
            "execution-gate",
            PolicyKind::ExecutableGate,
            PolicyScope::repository(&repository),
            "run the required checks",
        ),
    );

    let error = registry
        .admit_execution(
            &context(&repository),
            &ExecutionPolicyRequirements::for_execution(),
        )
        .expect_err("execution must not proceed without repository authority");

    assert_eq!(
        error,
        PolicyAdmissionError::MissingAuthority {
            authority: Authority::Repository,
        }
    );
    assert!(
        error.to_string().contains("no applicable source"),
        "missing-authority diagnostics must identify the fail-closed reason"
    );
}

#[test]
fn ambiguous_required_authority_rejects_execution_deterministically() {
    let fixture = tempfile::tempdir().expect("isolated policy fixture");
    let repository = fixture.path().join("repository");
    let target = context(&repository);

    let build_registry = |first: &str, second: &str| {
        let mut registry = AuthorityRegistry::new();
        insert(
            &mut registry,
            source(
                first,
                PolicyKind::RepositoryInstructions,
                PolicyScope::repository(&repository),
                "run the required checks",
            ),
        );
        insert(
            &mut registry,
            source(
                second,
                PolicyKind::RepositoryInstructions,
                PolicyScope::repository(&repository),
                "skip the required checks",
            ),
        );
        registry
    };

    let requirements = ExecutionPolicyRequirements::new([Authority::Repository]);
    let first = build_registry("z-source", "a-source")
        .admit_execution(&target, &requirements)
        .expect_err("conflicting repository authority must reject execution");
    let second = build_registry("a-source", "z-source")
        .admit_execution(&target, &requirements)
        .expect_err("insertion order must not authorize execution");

    assert_eq!(first, second);
    assert!(matches!(
        first,
        PolicyAdmissionError::AmbiguousAuthority {
            authority: Authority::Repository,
            sources,
            ..
        } if sources.iter().map(|source| source.as_str()).collect::<Vec<_>>()
            == vec!["a-source", "z-source"]
    ));
}

#[test]
fn complete_required_authority_set_admits_execution() {
    let fixture = tempfile::tempdir().expect("isolated policy fixture");
    let repository = fixture.path().join("repository");
    let registry = complete_registry(&repository);

    let admitted = registry
        .admit_execution(
            &context(&repository),
            &ExecutionPolicyRequirements::for_execution(),
        )
        .expect("complete, unambiguous policy should admit execution");

    assert_eq!(
        admitted.source_ids(),
        vec![
            "external-safety",
            "repository-instructions",
            "adapter-instructions",
            "execution-gate",
        ]
    );
}
