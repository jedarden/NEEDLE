#[test]
fn queries_the_frontier() {
    let output = harness::run("br ready --limit 1");
    assert!(output.is_ok());
}
