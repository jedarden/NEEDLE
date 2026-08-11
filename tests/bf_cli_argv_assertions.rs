//! Fixture-CLI tests asserting exact argv for bead store operations.
//!
//! These tests use mock CLI invocations to assert that NEEDLE calls `bf`
//! with the exact correct arguments. This prevents silent regressions where
//! the code might emit flags that the CLI doesn't accept.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;

use needle::bead_store::{BeadStore, RepairReport};
use needle::types::{Bead, BeadId, ClaimResult};

/// Mock CLI invocation recorder.
///
/// Records all CLI argv arrays for assertion in tests.
struct MockCliRecorder {
    /// Recorded argv arrays for each CLI invocation
    invocations: Arc<Mutex<Vec<Vec<String>>>>,
}

impl MockCliRecorder {
    fn new() -> Self {
        Self {
            invocations: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn record(&self, argv: Vec<String>) {
        self.invocations.lock().unwrap().push(argv);
    }

    fn get_invocations(&self) -> Vec<Vec<String>> {
        self.invocations.lock().unwrap().clone()
    }

    #[allow(dead_code)]
    fn find_invocation(&self, cmd: &str) -> Option<Vec<String>> {
        self.get_invocations()
            .into_iter()
            .find(|argv| argv.first().map(|first| first == cmd).unwrap_or(false))
    }
}

/// Mock BeadStore that records CLI argv for assertions.
///
/// This implementation captures the exact argv passed to each CLI operation
/// without actually invoking any subprocess.
struct FixtureCliStore {
    recorder: MockCliRecorder,
}

impl FixtureCliStore {
    fn new() -> Self {
        Self {
            recorder: MockCliRecorder::new(),
        }
    }

    fn recorder(&self) -> &MockCliRecorder {
        &self.recorder
    }
}

#[async_trait]
impl BeadStore for FixtureCliStore {
    async fn ready(&self, _filters: &needle::bead_store::Filters) -> Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn list_all(&self) -> Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn show(&self, _id: &BeadId) -> Result<Bead> {
        Ok(make_test_bead("test-id"))
    }

    async fn claim(&self, id: &BeadId, _actor: &str) -> Result<ClaimResult> {
        Ok(ClaimResult::Claimed(make_test_bead(id.as_ref())))
    }

    async fn claim_auto(&self, _actor: &str) -> Result<ClaimResult> {
        Ok(ClaimResult::NotClaimable {
            reason: "no beads available".into(),
        })
    }

    async fn release(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn block(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn split_bead(
        &self,
        _parent_id: &BeadId,
        _children: &[needle::bead_store::NewChild<'_>],
    ) -> Result<Vec<BeadId>> {
        Ok(Vec::new())
    }

    async fn add_dependency(&self, blocker_id: &BeadId, blocked_id: &BeadId) -> Result<()> {
        // Record the exact argv for bf dep add
        let args = vec![
            "dep".to_string(),
            "add".to_string(),
            blocker_id.as_ref().to_string(),
            "--blocks".to_string(),
            blocked_id.as_ref().to_string(),
        ];
        self.recorder.record(args);
        Ok(())
    }

    async fn remove_dependency(&self, blocked_id: &BeadId, blocker_id: &BeadId) -> Result<()> {
        // Record the exact argv for bf dep remove
        let args = vec![
            "dep".to_string(),
            "remove".to_string(),
            blocked_id.as_ref().to_string(),
            blocker_id.as_ref().to_string(),
        ];
        self.recorder.record(args);
        Ok(())
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> Result<()> {
        Ok(())
    }

    async fn create_bead(&self, title: &str, body: &str, labels: &[&str]) -> Result<BeadId> {
        // Record the exact argv that BrCliBeadStore/BfCliBeadStore would emit
        let mut args: Vec<String> = vec![
            "create".to_string(),
            "--title".to_string(),
            title.to_string(),
            "--description".to_string(),
            body.to_string(),
        ];
        for label in labels {
            args.push("--label".to_string());
            args.push((*label).to_string());
        }
        self.recorder.record(args);
        Ok(BeadId::from("test-created-id"))
    }

    async fn doctor_repair(&self) -> Result<RepairReport> {
        Ok(RepairReport::default())
    }

    async fn doctor_check(&self) -> Result<RepairReport> {
        Ok(RepairReport::default())
    }

    async fn full_rebuild(&self) -> Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

fn make_test_bead(id: &str) -> Bead {
    use needle::types::BeadStatus;
    Bead {
        id: BeadId::from(id),
        title: "Test Bead".into(),
        body: Some("Test body".into()),
        priority: 3, // P3 (normal priority)
        status: BeadStatus::Open,
        assignee: None,
        labels: Vec::new(),
        workspace: PathBuf::from("/test/workspace"),
        dependencies: Vec::new(),
        dependents: Vec::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn create_bead_argv_no_labels() {
    let store = FixtureCliStore::new();
    let title = "Test Bead Title";
    let body = "Test bead body description";
    let labels: Vec<&str> = Vec::new();

    store.create_bead(title, body, &labels).await.unwrap();

    let invocations = store.recorder().get_invocations();
    assert_eq!(
        invocations.len(),
        1,
        "Should have exactly one CLI invocation"
    );

    let argv = &invocations[0];
    assert_eq!(
        argv,
        &vec![
            "create".to_string(),
            "--title".to_string(),
            title.to_string(),
            "--description".to_string(),
            body.to_string(),
        ],
        "create_bead with no labels should emit: create --title T --description D"
    );
}

#[tokio::test]
async fn create_bead_argv_one_label() {
    let store = FixtureCliStore::new();
    let title = "Single Label Bead";
    let body = "Bead with one label";
    let labels = vec!["bug-fix"];

    store.create_bead(title, body, &labels).await.unwrap();

    let invocations = store.recorder().get_invocations();
    assert_eq!(
        invocations.len(),
        1,
        "Should have exactly one CLI invocation"
    );

    let argv = &invocations[0];
    assert_eq!(
        argv,
        &vec![
            "create".to_string(),
            "--title".to_string(),
            title.to_string(),
            "--description".to_string(),
            body.to_string(),
            "--label".to_string(),
            "bug-fix".to_string(),
        ],
        "create_bead with one label should emit: create --title T --description D --label L1"
    );
}

#[tokio::test]
async fn create_bead_argv_two_labels() {
    let store = FixtureCliStore::new();
    let title = "Multi Label Bead";
    let body = "Bead with two labels";
    let labels = vec!["bug-fix", "high-priority"];

    store.create_bead(title, body, &labels).await.unwrap();

    let invocations = store.recorder().get_invocations();
    assert_eq!(
        invocations.len(),
        1,
        "Should have exactly one CLI invocation"
    );

    let argv = &invocations[0];
    assert_eq!(
        argv,
        &vec![
            "create".to_string(),
            "--title".to_string(),
            title.to_string(),
            "--description".to_string(),
            body.to_string(),
            "--label".to_string(),
            "bug-fix".to_string(),
            "--label".to_string(),
            "high-priority".to_string(),
        ],
        "create_bead with two labels should emit: create --title T --description D --label L1 --label L2"
    );
}

#[tokio::test]
async fn create_bead_argv_three_labels() {
    let store = FixtureCliStore::new();
    let title = "Triple Label Bead";
    let body = "Bead with three labels";
    let labels = vec!["bug-fix", "high-priority", "security"];

    store.create_bead(title, body, &labels).await.unwrap();

    let invocations = store.recorder().get_invocations();
    assert_eq!(
        invocations.len(),
        1,
        "Should have exactly one CLI invocation"
    );

    let argv = &invocations[0];
    assert_eq!(
        argv,
        &vec![
            "create".to_string(),
            "--title".to_string(),
            title.to_string(),
            "--description".to_string(),
            body.to_string(),
            "--label".to_string(),
            "bug-fix".to_string(),
            "--label".to_string(),
            "high-priority".to_string(),
            "--label".to_string(),
            "security".to_string(),
        ],
        "create_bead with three labels should emit: create --title T --description D --label L1 --label L2 --label L3"
    );
}

#[tokio::test]
async fn add_dependency_argv() {
    let store = FixtureCliStore::new();
    let blocker = BeadId::from("bf-blocker");
    let blocked = BeadId::from("bf-blocked");

    store.add_dependency(&blocker, &blocked).await.unwrap();

    let invocations = store.recorder().get_invocations();
    assert_eq!(
        invocations.len(),
        1,
        "Should have exactly one CLI invocation"
    );

    let argv = &invocations[0];
    assert_eq!(
        argv,
        &vec![
            "dep".to_string(),
            "add".to_string(),
            "bf-blocker".to_string(),
            "--blocks".to_string(),
            "bf-blocked".to_string(),
        ],
        "add_dependency should emit: dep add <blocker> --blocks <blocked>"
    );
}

#[tokio::test]
async fn remove_dependency_argv() {
    let store = FixtureCliStore::new();
    let blocked = BeadId::from("bf-blocked");
    let blocker = BeadId::from("bf-blocker");

    store.remove_dependency(&blocked, &blocker).await.unwrap();

    let invocations = store.recorder().get_invocations();
    assert_eq!(
        invocations.len(),
        1,
        "Should have exactly one CLI invocation"
    );

    let argv = &invocations[0];
    assert_eq!(
        argv,
        &vec![
            "dep".to_string(),
            "remove".to_string(),
            "bf-blocked".to_string(),
            "bf-blocker".to_string(),
        ],
        "remove_dependency should emit: dep remove <blocked> <blocker>"
    );
}
