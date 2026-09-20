use needle::policy::{
    AuthorityRegistry, PolicyError, PolicyKind, PolicyScope, PolicySource, ResolutionContext,
};

fn source(id: &str, kind: PolicyKind, scope: PolicyScope, content: &str) -> PolicySource {
    PolicySource::new(id, kind, scope, content).expect("fixture source is valid")
}

fn canonical_with_sources(sources: Vec<PolicySource>) -> (String, String) {
    let mut registry = AuthorityRegistry::new();
    for source in sources {
        registry
            .insert(source)
            .expect("fixture source IDs are unique");
    }

    let resolved = registry
        .resolve(&ResolutionContext::new("/fixture/repository/src/lib.rs").for_adapter("codex"))
        .expect("fixture policy is unambiguous");
    (resolved.canonical_json().unwrap(), resolved.hash().unwrap())
}

fn effective_sources(content: &str) -> Vec<PolicySource> {
    vec![
        source(
            "repository",
            PolicyKind::RepositoryInstructions,
            PolicyScope::repository("/fixture/repository"),
            content,
        ),
        source(
            "global-plan",
            PolicyKind::CurrentPlan,
            PolicyScope::Global,
            "implement the acceptance contract",
        ),
        source(
            "adapter",
            PolicyKind::AdapterInstructions,
            PolicyScope::adapter("codex"),
            "use the explicit context bundle",
        ),
    ]
}

#[test]
fn canonical_policy_inputs_are_order_independent_and_hashable() {
    let mut forward = effective_sources("preserve shared checkout changes");
    let mut reverse = forward.clone();
    reverse.reverse();

    let forward = canonical_with_sources(std::mem::take(&mut forward));
    let reverse = canonical_with_sources(reverse);

    assert_eq!(forward.0, reverse.0);
    assert_eq!(forward.1, reverse.1);
    assert_eq!(forward.1.len(), 64);
    assert!(forward.0.contains("\"content_sha256\""));
    assert!(!forward.0.contains("preserve shared checkout changes"));
}

#[test]
fn only_effective_policy_changes_affect_the_canonical_hash() {
    let mut with_irrelevant = effective_sources("preserve shared checkout changes");
    with_irrelevant.push(source(
        "other-repository",
        PolicyKind::ExternalConstraint,
        PolicyScope::repository("/fixture/other-repository"),
        "this source is out of scope",
    ));
    with_irrelevant.push(source(
        "wrong-adapter",
        PolicyKind::AdapterInstructions,
        PolicyScope::adapter("claude"),
        "this source is out of scope",
    ));

    let baseline = canonical_with_sources(effective_sources("preserve shared checkout changes"));
    let irrelevant = canonical_with_sources(with_irrelevant);
    let changed = canonical_with_sources(effective_sources("use a different instruction"));

    assert_eq!(baseline.1, irrelevant.1);
    assert_ne!(baseline.1, changed.1);
}

#[test]
fn conflicting_effective_inputs_fail_deterministically_before_hashing() {
    let build_registry = |first: &str, second: &str| {
        let mut registry = AuthorityRegistry::new();
        registry
            .insert(source(
                first,
                PolicyKind::RepositoryInstructions,
                PolicyScope::repository("/fixture/repository"),
                "run the required checks",
            ))
            .unwrap();
        registry
            .insert(source(
                second,
                PolicyKind::RepositoryInstructions,
                PolicyScope::repository("/fixture/repository"),
                "skip the required checks",
            ))
            .unwrap();
        registry
    };

    let context = ResolutionContext::new("/fixture/repository/src/lib.rs");
    let first = build_registry("z-source", "a-source")
        .resolve(&context)
        .expect_err("conflicting sources must not be hashed");
    let second = build_registry("a-source", "z-source")
        .resolve(&context)
        .expect_err("insertion order must not affect the failure");

    assert_eq!(first, second);
    assert_eq!(
        first,
        PolicyError::Conflict {
            authority: needle::policy::Authority::Repository,
            scope: PolicyScope::repository("/fixture/repository"),
            sources: vec![
                needle::policy::PolicySourceId::new("a-source").unwrap(),
                needle::policy::PolicySourceId::new("z-source").unwrap(),
            ],
        }
    );
}
