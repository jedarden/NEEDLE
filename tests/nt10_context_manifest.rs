use needle::needle_learning::{AttemptId, CURRENT_SCHEMA_VERSION};
use needle::policy::{
    AuthorityRegistry, PolicyError, PolicyKind, PolicyScope, PolicySource, ResolutionContext,
    CONTEXT_MANIFEST_SCHEMA_VERSION, RESOLVED_POLICY_SOURCE_ID, RESOLVED_POLICY_VERSION,
};

fn source(id: &str, kind: PolicyKind, scope: PolicyScope, content: &str) -> PolicySource {
    PolicySource::new(id, kind, scope, content).expect("fixture source is valid")
}

fn resolved_registry(repository: &std::path::Path) -> AuthorityRegistry {
    let mut registry = AuthorityRegistry::new();
    registry
        .insert(source(
            "repository-instructions",
            PolicyKind::RepositoryInstructions,
            PolicyScope::repository(repository),
            "preserve unrelated shared-checkout changes",
        ))
        .expect("fixture source IDs are unique");
    registry
        .insert(source(
            "adapter-instructions",
            PolicyKind::AdapterInstructions,
            PolicyScope::adapter("codex"),
            "use the explicit context bundle",
        ))
        .expect("fixture source IDs are unique");
    registry
}

#[test]
fn materializes_a_context_manifest_from_the_resolved_policy_snapshot() {
    let fixture = tempfile::tempdir().expect("isolated fixture directory");
    let repository = fixture.path().join("repository");
    let target = repository.join("src/lib.rs");
    let context = ResolutionContext::new(&target).for_adapter("codex");
    let registry = resolved_registry(&repository);
    let attempt_id = AttemptId::new("attempt-nt10-context-manifest").unwrap();
    let resolved = registry
        .resolve(&context)
        .expect("fixture policy is resolvable");

    let manifest = resolved
        .materialize_context_manifest(attempt_id.clone())
        .expect("resolved policy materializes");

    assert_eq!(manifest.schema_version, CONTEXT_MANIFEST_SCHEMA_VERSION);
    assert_eq!(manifest.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(manifest.attempt_id, attempt_id);
    assert_eq!(
        manifest.policy.source_id.as_str(),
        RESOLVED_POLICY_SOURCE_ID
    );
    assert_eq!(manifest.policy.version, RESOLVED_POLICY_VERSION);
    assert_eq!(manifest.policy.digest.as_str(), resolved.hash().unwrap());
    assert!(manifest.tools.is_empty());
    assert!(manifest.memory.is_empty());
    assert!(manifest.redactions.is_empty());
    manifest
        .validate_for(&manifest.attempt_id)
        .expect("materialized manifest has the expected schema and owner");

    let encoded = serde_json::to_string(&manifest).expect("manifest is serializable");
    assert!(!encoded.contains("preserve unrelated shared-checkout changes"));
    assert!(!encoded.contains("use the explicit context bundle"));
}

#[test]
fn conflicting_policy_snapshot_fails_deterministically_before_materialization() {
    let fixture = tempfile::tempdir().expect("isolated fixture directory");
    let repository = fixture.path().join("repository");
    let target = repository.join("src/lib.rs");
    let context = ResolutionContext::new(&target);

    let build_registry = |first: &str, second: &str| {
        let mut registry = AuthorityRegistry::new();
        registry
            .insert(source(
                first,
                PolicyKind::RepositoryInstructions,
                PolicyScope::repository(&repository),
                "run the required checks",
            ))
            .expect("fixture source IDs are unique");
        registry
            .insert(source(
                second,
                PolicyKind::RepositoryInstructions,
                PolicyScope::repository(&repository),
                "skip the required checks",
            ))
            .expect("fixture source IDs are unique");
        registry
    };

    let attempt_id = AttemptId::new("attempt-nt10-conflict").unwrap();
    let first = build_registry("z-source", "a-source")
        .materialize_context_manifest(&context, attempt_id.clone())
        .expect_err("conflicting policy must not materialize");
    let second = build_registry("a-source", "z-source")
        .materialize_context_manifest(&context, attempt_id)
        .expect_err("conflicting policy must not materialize");

    assert_eq!(first, second);
    assert!(matches!(
        first,
        PolicyError::Conflict {
            sources,
            ..
        } if sources.iter().map(|source| source.as_str()).collect::<Vec<_>>()
            == vec!["a-source", "z-source"]
    ));
}
