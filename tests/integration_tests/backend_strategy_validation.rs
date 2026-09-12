use needle::bead_store::{
    validate_strategy_name, ClaimAutoStrategy, ClaimStrategy, CreateIdStrategy, ImportStrategy,
    LabelsStrategy, ParsedStrategy, SplitStrategy,
};
use std::path::Path;

#[test]
fn validates_all_descriptor_strategy_operations() {
    let descriptor = Path::new("/tmp/backends/example.yaml");
    let cases = [
        (
            "claim",
            "compare_and_set",
            ParsedStrategy::Claim(ClaimStrategy::CompareAndSet),
        ),
        (
            "claim_auto",
            "atomic_subcommand",
            ParsedStrategy::ClaimAuto(ClaimAutoStrategy::AtomicSubcommand),
        ),
        (
            "split",
            "sequential",
            ParsedStrategy::Split(SplitStrategy::Sequential),
        ),
        (
            "create_id",
            "bare_id",
            ParsedStrategy::CreateId(CreateIdStrategy::BareId),
        ),
        (
            "labels",
            "repeated",
            ParsedStrategy::Labels(LabelsStrategy::Repeated),
        ),
        (
            "import",
            "input_plus_mode",
            ParsedStrategy::Import(ImportStrategy::InputPlusMode),
        ),
    ];

    for (operation, strategy, expected) in cases {
        assert_eq!(
            validate_strategy_name(descriptor, operation, strategy).unwrap(),
            expected
        );
    }
}

#[test]
fn invalid_strategy_reports_value_operation_and_descriptor() {
    let error = validate_strategy_name(Path::new("/tmp/backends/descriptor.yaml"), "claim", "foo")
        .unwrap_err()
        .to_string();

    assert_eq!(
        error,
        "unknown strategy 'foo' for operation 'claim' in /tmp/backends/descriptor.yaml"
    );
}

#[test]
fn invalid_operation_reports_operation_and_descriptor() {
    let error = validate_strategy_name(
        Path::new("/tmp/backends/descriptor.yaml"),
        "teleport",
        "atomic",
    )
    .unwrap_err()
    .to_string();

    assert_eq!(
        error,
        "unknown strategy operation 'teleport' in /tmp/backends/descriptor.yaml"
    );
}
