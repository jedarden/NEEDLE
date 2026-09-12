use async_trait::async_trait;
use needle::bead_store::{
    execute_create_id_strategy, execute_split_strategy, CreateIdStrategy, NewChild,
    SequentialSplitError, SplitStrategy, SplitStrategyOperations,
};
use needle::types::BeadId;
use std::sync::Mutex;

#[derive(Default)]
struct MockSplitOperations {
    events: Mutex<Vec<String>>,
    created: Mutex<usize>,
    links: Mutex<usize>,
    fail_link_at: Option<usize>,
}

#[async_trait]
impl SplitStrategyOperations for MockSplitOperations {
    async fn transactional_split(
        &self,
        _parent_id: &BeadId,
        children: &[NewChild<'_>],
    ) -> anyhow::Result<Vec<BeadId>> {
        self.events.lock().unwrap().push("transaction".to_string());
        Ok((0..children.len())
            .map(|index| BeadId::from(format!("child-{index}")))
            .collect())
    }

    async fn create_split_child(
        &self,
        title: &str,
        _body: &str,
        _labels: &[&str],
    ) -> anyhow::Result<BeadId> {
        let mut created = self.created.lock().unwrap();
        let index = *created;
        *created += 1;
        self.events.lock().unwrap().push(format!("create:{title}"));
        Ok(BeadId::from(format!("child-{index}")))
    }

    async fn link_split_child(&self, child_id: &BeadId, _parent_id: &BeadId) -> anyhow::Result<()> {
        let mut links = self.links.lock().unwrap();
        let index = *links;
        *links += 1;
        self.events.lock().unwrap().push(format!("link:{child_id}"));
        if self.fail_link_at == Some(index) {
            anyhow::bail!("injected link failure");
        }
        Ok(())
    }
}

fn children<'a>(labels: &'a [&'a str]) -> [NewChild<'a>; 2] {
    [
        NewChild {
            title: "first",
            body: "body one",
            labels,
        },
        NewChild {
            title: "second",
            body: "body two",
            labels,
        },
    ]
}

#[tokio::test]
async fn transactional_split_uses_exactly_one_backend_transaction() {
    let operations = MockSplitOperations::default();
    let labels = ["split-child"];

    let ids = execute_split_strategy(
        &operations,
        SplitStrategy::TransactionalBatch,
        &BeadId::from("parent"),
        &children(&labels),
    )
    .await
    .unwrap();

    assert_eq!(ids.len(), 2);
    assert_eq!(*operations.events.lock().unwrap(), ["transaction"]);
}

#[tokio::test]
async fn sequential_split_commits_each_create_then_link_in_order() {
    let operations = MockSplitOperations::default();
    let labels = ["split-child"];

    let ids = execute_split_strategy(
        &operations,
        SplitStrategy::Sequential,
        &BeadId::from("parent"),
        &children(&labels),
    )
    .await
    .unwrap();

    assert_eq!(ids, [BeadId::from("child-0"), BeadId::from("child-1")]);
    assert_eq!(
        *operations.events.lock().unwrap(),
        [
            "create:first",
            "link:child-0",
            "create:second",
            "link:child-1"
        ]
    );
}

#[tokio::test]
async fn sequential_failure_reports_every_already_committed_child() {
    let operations = MockSplitOperations {
        fail_link_at: Some(1),
        ..Default::default()
    };
    let labels = ["split-child"];

    let error = execute_split_strategy(
        &operations,
        SplitStrategy::Sequential,
        &BeadId::from("parent"),
        &children(&labels),
    )
    .await
    .unwrap_err();
    let split_error = error.downcast_ref::<SequentialSplitError>().unwrap();

    assert_eq!(
        split_error.created,
        [BeadId::from("child-0"), BeadId::from("child-1")]
    );
    assert!(error.to_string().contains("after creating 2 child bead(s)"));
}

#[test]
fn create_id_strategies_parse_bare_direct_and_enveloped_ids() {
    assert_eq!(
        execute_create_id_strategy(CreateIdStrategy::BareId, " child-1\n").unwrap(),
        "child-1"
    );
    assert_eq!(
        execute_create_id_strategy(CreateIdStrategy::JsonField, r#"{"id":"child-2"}"#).unwrap(),
        "child-2"
    );
    assert_eq!(
        execute_create_id_strategy(
            CreateIdStrategy::JsonField,
            r#"{"version":1,"data":{"id":"child-3"}}"#,
        )
        .unwrap(),
        "child-3"
    );
}

#[test]
fn create_id_json_rejects_missing_non_string_and_empty_fields() {
    for output in [r#"{}"#, r#"{"id":7}"#, r#"{"id":""}"#] {
        let error = execute_create_id_strategy(CreateIdStrategy::JsonField, output).unwrap_err();
        assert!(error
            .to_string()
            .contains("missing non-empty string 'id' field"));
    }
}
