use needle::bead_store::{
    execute_import_strategy, execute_labels_strategy, parse_labels_strategy, ImportStrategy,
    LabelsStrategy,
};
use std::path::Path;

#[test]
fn csv_labels_parse_quotes_commas_escapes_empty_fields_and_whitespace() {
    let labels = parse_labels_strategy(
        LabelsStrategy::Csv,
        &[r#" bug, "needs,review", "quoted ""label""", , docs "#],
    )
    .unwrap();

    assert_eq!(labels, ["bug", "needs,review", "quoted \"label\"", "docs"]);
    assert!(parse_labels_strategy(LabelsStrategy::Csv, &[r#"bug,"broken"#]).is_err());
}

#[test]
fn csv_formatting_round_trips_labels_that_need_quoting() {
    let source = ["bug", "needs,review", "quoted \"label\"", " spaced "];
    let args = execute_labels_strategy(LabelsStrategy::Csv, &source);

    assert_eq!(args[0], "--labels");
    assert_eq!(
        parse_labels_strategy(LabelsStrategy::Csv, &[&args[1]]).unwrap(),
        source
    );
}

#[test]
fn repeated_labels_accumulate_occurrences_and_ignore_empty_values() {
    let labels = parse_labels_strategy(
        LabelsStrategy::Repeated,
        &[" bug ", "", "needs,review", "   ", "docs"],
    )
    .unwrap();

    assert_eq!(labels, ["bug", "needs,review", "docs"]);
    assert_eq!(
        execute_labels_strategy(
            LabelsStrategy::Repeated,
            &labels.iter().map(String::as_str).collect::<Vec<_>>()
        ),
        [
            "--label",
            "bug",
            "--label",
            "needs,review",
            "--label",
            "docs"
        ]
    );
}

#[test]
fn bare_import_passes_only_the_input_path() {
    assert_eq!(
        execute_import_strategy(ImportStrategy::Bare, Path::new("checkpoint.jsonl"), None).unwrap(),
        ["checkpoint.jsonl"]
    );
}

#[test]
fn input_plus_mode_requires_and_appends_an_explicit_mode() {
    assert_eq!(
        execute_import_strategy(
            ImportStrategy::InputPlusMode,
            Path::new("checkpoint/"),
            Some("--restore-into-empty")
        )
        .unwrap(),
        ["--input", "checkpoint/", "--restore-into-empty"]
    );
    assert_eq!(
        execute_import_strategy(
            ImportStrategy::InputPlusMode,
            Path::new("checkpoint.jsonl"),
            Some("override")
        )
        .unwrap(),
        ["--input", "checkpoint.jsonl", "--mode=override"]
    );
    assert!(execute_import_strategy(
        ImportStrategy::InputPlusMode,
        Path::new("checkpoint.jsonl"),
        None
    )
    .is_err());
}
