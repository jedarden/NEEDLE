//! End-to-end CI red filing against a local fake Forgejo status API.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use needle::bead_store::{BeadStore, DedupCreate, Filters, RepairReport};
use needle::config::CiWatchConfig;
use needle::strand::ci_watch::{CiVerdictSource, CiWatchStrand, ForgejoFirstVerdictSource};
use needle::strand::Strand;
use needle::telemetry::Telemetry;
use needle::types::{Bead, BeadId, BeadStatus, ClaimResult, StrandResult};
use serde_json::Value;

const ORIGIN_URL: &str = "https://forgejo.invalid/jedarden/NEEDLE.git";

static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

struct IsolatedHome {
    _lock: MutexGuard<'static, ()>,
    previous_home: Option<OsString>,
}

impl IsolatedHome {
    fn pin(home: &Path) -> Self {
        let lock = HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        Self {
            _lock: lock,
            previous_home,
        }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match &self.previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[derive(Debug, Clone)]
struct FiledBead {
    title: String,
    body: String,
    labels: Vec<String>,
    priority: u8,
    status: BeadStatus,
    notes: String,
}

#[derive(Default)]
struct StoreState {
    beads: HashMap<String, FiledBead>,
    unique_refs: HashMap<String, String>,
    next_id: usize,
}

#[derive(Default)]
struct FakeStore {
    state: Mutex<StoreState>,
}

impl FakeStore {
    fn bead_count(&self) -> usize {
        self.state.lock().unwrap().beads.len()
    }

    fn beads(&self) -> Vec<(BeadId, FiledBead)> {
        self.state
            .lock()
            .unwrap()
            .beads
            .iter()
            .map(|(id, bead)| (BeadId::from(id.clone()), bead.clone()))
            .collect()
    }
}

#[async_trait]
impl BeadStore for FakeStore {
    async fn ready(&self, _filters: &Filters) -> anyhow::Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn list_all(&self) -> anyhow::Result<Vec<Bead>> {
        Ok(Vec::new())
    }

    async fn show(&self, id: &BeadId) -> anyhow::Result<Bead> {
        let state = self.state.lock().unwrap();
        let filed = state
            .beads
            .get(id.as_ref())
            .ok_or_else(|| anyhow::anyhow!("unknown fake bead {id}"))?;
        Ok(Bead {
            id: id.clone(),
            title: filed.title.clone(),
            body: Some(filed.body.clone()),
            priority: filed.priority,
            status: filed.status.clone(),
            assignee: None,
            labels: filed.labels.clone(),
            workspace: PathBuf::new(),
            dependencies: Vec::new(),
            dependents: Vec::new(),
            comments: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
    }

    async fn claim(&self, _id: &BeadId, _actor: &str) -> anyhow::Result<ClaimResult> {
        anyhow::bail!("claim is unused in this test")
    }

    async fn claim_auto(&self, _actor: &str) -> anyhow::Result<ClaimResult> {
        anyhow::bail!("claim_auto is unused in this test")
    }

    async fn release(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn block(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn clear_assignee(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn labels(&self, id: &BeadId) -> anyhow::Result<Vec<String>> {
        Ok(self.show(id).await?.labels)
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn create_bead(
        &self,
        _title: &str,
        _body: &str,
        _labels: &[&str],
    ) -> anyhow::Result<BeadId> {
        anyhow::bail!("unkeyed create is unused in this test")
    }

    async fn create_bead_with_unique_ref(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
        priority: u8,
        unique_ref: &str,
    ) -> anyhow::Result<DedupCreate> {
        let mut state = self.state.lock().unwrap();
        if let Some(id) = state.unique_refs.get(unique_ref) {
            return Ok(DedupCreate::Existed(BeadId::from(id.clone())));
        }
        state.next_id += 1;
        let id = format!("fake-ci-{}", state.next_id);
        state.unique_refs.insert(unique_ref.to_string(), id.clone());
        state.beads.insert(
            id.clone(),
            FiledBead {
                title: title.to_string(),
                body: body.to_string(),
                labels: labels.iter().map(|label| (*label).to_string()).collect(),
                priority,
                status: BeadStatus::Open,
                notes: String::new(),
            },
        );
        Ok(DedupCreate::Created(BeadId::from(id)))
    }

    async fn append_notes(&self, id: &BeadId, note: &str) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        let bead = state
            .beads
            .get_mut(id.as_ref())
            .ok_or_else(|| anyhow::anyhow!("unknown fake bead {id}"))?;
        if !bead.notes.is_empty() {
            bead.notes.push('\n');
        }
        bead.notes.push_str(note);
        Ok(())
    }

    async fn add_dependency(&self, _blocker: &BeadId, _blocked: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn remove_dependency(&self, _blocked: &BeadId, _blocker: &BeadId) -> anyhow::Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> anyhow::Result<RepairReport> {
        Ok(RepairReport::default())
    }

    async fn doctor_check(&self) -> anyhow::Result<RepairReport> {
        Ok(RepairReport::default())
    }

    async fn full_rebuild(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn has_valid_store(&self) -> bool {
        true
    }
}

struct FakeForgejo {
    base_url: String,
    statuses: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    stopped: Arc<AtomicBool>,
    server: Option<JoinHandle<()>>,
}

impl FakeForgejo {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let statuses = Arc::new(Mutex::new(HashMap::<String, Vec<Value>>::new()));
        let server_statuses = statuses.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let server_stopped = stopped.clone();
        let server = thread::spawn(move || {
            while !server_stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => serve_status_request(stream, &server_statuses),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base_url: format!("http://{address}"),
            statuses,
            stopped,
            server: Some(server),
        }
    }

    fn set_status(&self, revision: &str, state: &str, description: &str) {
        self.statuses.lock().unwrap().insert(
            revision.to_string(),
            vec![serde_json::json!({
                "state": state,
                "context": "iad-ci/needle-ci",
                "sha": revision,
                "description": description,
                "created_at": "2026-09-27T12:00:00Z",
                "updated_at": "2026-09-27T12:00:00Z"
            })],
        );
    }
}

impl Drop for FakeForgejo {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

fn serve_status_request(mut stream: TcpStream, statuses: &Mutex<HashMap<String, Vec<Value>>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut request_line = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        if reader.read_line(&mut request_line).is_err() {
            return;
        }
        let mut header = String::new();
        loop {
            header.clear();
            match reader.read_line(&mut header) {
                Ok(0) | Err(_) => return,
                Ok(_) if header == "\r\n" || header == "\n" => break,
                Ok(_) => {}
            }
        }
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or_default();
    let revision = path
        .split("/commits/")
        .nth(1)
        .and_then(|tail| tail.split('/').next())
        .unwrap_or_default();
    let body = serde_json::to_vec(
        &statuses
            .lock()
            .unwrap()
            .get(revision)
            .cloned()
            .unwrap_or_default(),
    )
    .unwrap();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(&body);
}

fn git(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn add_commit(home: &Path, workspace: &Path, content: &str) -> String {
    std::fs::write(workspace.join("status.txt"), content).unwrap();
    git(home, workspace, &["add", "status.txt"]);
    git(home, workspace, &["commit", "-m", content]);
    let revision = git(home, workspace, &["rev-parse", "HEAD"]);
    git(home, workspace, &["push", "origin", "main"]);
    revision
}

#[tokio::test]
async fn fake_forgejo_reds_dedupe_by_fingerprint_and_green_takes_no_action() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    let workspace = fixture.path().join("workspace");
    let remote = fixture.path().join("remote.git");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    // The fixture workspace is the only scan root and every git child process
    // receives a disposable home instead of reading production git config.
    let _isolated_home = IsolatedHome::pin(&home);

    git(
        &home,
        fixture.path(),
        &["init", "--bare", remote.to_str().unwrap()],
    );
    git(&home, &workspace, &["init", "--initial-branch=main"]);
    git(
        &home,
        &workspace,
        &["config", "user.name", "NEEDLE CI test"],
    );
    git(
        &home,
        &workspace,
        &["config", "user.email", "needle-ci-test@example.invalid"],
    );
    git(&home, &workspace, &["remote", "add", "origin", ORIGIN_URL]);
    let file_remote = format!("url.file://{}/.insteadOf", remote.display());
    git(
        &home,
        &workspace,
        &["config", "--add", &file_remote, ORIGIN_URL],
    );

    let forgejo = FakeForgejo::start();
    let base_revision = add_commit(&home, &workspace, "base");
    forgejo.set_status(&base_revision, "success", "needle-ci Succeeded");

    let source: Arc<dyn CiVerdictSource> =
        Arc::new(ForgejoFirstVerdictSource::new(forgejo.base_url.clone()));
    let state_dir = fixture.path().join("state").join("ci_watch");
    let strand = CiWatchStrand::with_source(
        CiWatchConfig {
            enabled: true,
            poll_interval_secs: 0,
        },
        workspace.clone(),
        state_dir,
        Telemetry::new("ci-watch-integration-test".to_string()),
        source,
    );
    let store = FakeStore::default();

    let first_red_revision = add_commit(&home, &workspace, "red one");
    forgejo.set_status(
        &first_red_revision,
        "failure",
        "needle-ci Failed: verify: 2 failed: tests::beta, tests::alpha",
    );
    let pending_tip_revision = add_commit(&home, &workspace, "pending successor");
    assert!(matches!(
        strand.evaluate(&store, &Default::default()).await,
        StrandResult::WorkCreated
    ));
    assert_eq!(
        store.bead_count(),
        1,
        "the newest completed red files even after main advances to an unverdicted tip"
    );
    assert!(store.beads()[0].1.body.contains(&first_red_revision));
    assert!(!store.beads()[0].1.body.contains(&pending_tip_revision));

    // The next poll can see the exact same Forgejo status again. It remains
    // one filing and does not add a duplicate recurrence note.
    assert!(matches!(
        strand.evaluate(&store, &Default::default()).await,
        StrandResult::NoWork
    ));
    assert_eq!(store.bead_count(), 1);
    assert!(store.beads()[0].1.notes.is_empty());

    // The branch advances to another red revision with the same unordered
    // failing test set. Its note is appended to the first bead.
    let recurring_red_revision = add_commit(&home, &workspace, "same red again");
    forgejo.set_status(
        &recurring_red_revision,
        "failure",
        "needle-ci Failed: verify: 2 failed: tests::alpha, tests::beta",
    );
    assert!(matches!(
        strand.evaluate(&store, &Default::default()).await,
        StrandResult::NoWork
    ));
    assert_eq!(
        store.bead_count(),
        1,
        "a recurrence does not create a duplicate"
    );
    let first_bead = store.beads().pop().unwrap();
    assert!(first_bead.1.notes.contains(&recurring_red_revision));

    let different_red_revision = add_commit(&home, &workspace, "different red");
    forgejo.set_status(
        &different_red_revision,
        "failure",
        "needle-ci Failed: verify: 1 failed: tests::gamma",
    );
    assert!(matches!(
        strand.evaluate(&store, &Default::default()).await,
        StrandResult::WorkCreated
    ));
    assert_eq!(
        store.bead_count(),
        2,
        "a different fingerprint files a second bead"
    );

    let green_revision = add_commit(&home, &workspace, "green");
    forgejo.set_status(&green_revision, "success", "needle-ci Succeeded");
    assert!(matches!(
        strand.evaluate(&store, &Default::default()).await,
        StrandResult::NoWork
    ));
    let beads = store.beads();
    assert_eq!(beads.len(), 2, "green does not create or close beads");
    assert!(beads
        .iter()
        .all(|(_, bead)| bead.status == BeadStatus::Open));
    let first = beads
        .iter()
        .find(|(_, bead)| bead.title.ends_with("tests::alpha"))
        .unwrap();
    assert_eq!(first.1.priority, 0, "red beads are P0");
    assert!(first.1.body.contains(&first_red_revision));
    assert!(first
        .1
        .body
        .contains("Failing tests: tests::alpha, tests::beta"));
    assert!(first.1.body.contains("Parent revision status: green"));
    assert!(first.1.body.contains("Workflow: iad-ci/needle-ci"));
    assert!(first.1.notes.contains("still red at"));
}
