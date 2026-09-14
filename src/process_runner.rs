//! Injectable captured-process boundary.
//!
//! Policy and adapter code describe a process invocation as data and receives
//! a normalized outcome. Production uses [`TokioProcessRunner`]; unit tests use
//! [`FakeProcessRunner`] without forking a child.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// One captured process invocation.
///
/// Deliberately does not implement `Debug`: arguments and environment values
/// can contain credentials and must never leak through generic diagnostics.
#[derive(Clone)]
pub struct ProcessRequest {
    program: PathBuf,
    args: Vec<OsString>,
    current_dir: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
}

impl ProcessRequest {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            current_dir: None,
            env: Vec::new(),
        }
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_os_string()));
        self
    }

    pub fn current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(path.into());
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.args
    }

    pub fn working_directory(&self) -> Option<&Path> {
        self.current_dir.as_deref()
    }
}

/// Normalized result of a captured process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessOutput {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ProcessOutput {
    pub fn success(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            success: true,
            exit_code: Some(0),
            stdout: stdout.into(),
            stderr: Vec::new(),
        }
    }

    pub fn failure(exit_code: Option<i32>, stderr: impl Into<Vec<u8>>) -> Self {
        Self {
            success: false,
            exit_code,
            stdout: Vec::new(),
            stderr: stderr.into(),
        }
    }
}

/// Runs a process to completion while capturing stdout and stderr.
#[async_trait]
pub trait ProcessRunner: Send + Sync {
    async fn output(&self, request: ProcessRequest, timeout: Duration) -> Result<ProcessOutput>;
}

/// Production captured-process adapter.
#[derive(Debug, Default)]
pub struct TokioProcessRunner;

#[async_trait]
impl ProcessRunner for TokioProcessRunner {
    async fn output(&self, request: ProcessRequest, timeout: Duration) -> Result<ProcessOutput> {
        const MAX_SPAWN_ATTEMPTS: u32 = 5;
        const ETXTBSY_BACKOFF: Duration = Duration::from_millis(20);

        let mut attempts = 0;
        let child = loop {
            attempts += 1;
            let mut command = tokio::process::Command::new(&request.program);
            command
                .args(&request.args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);
            if let Some(current_dir) = &request.current_dir {
                command.current_dir(current_dir);
            }
            command.envs(request.env.iter().cloned());

            match command.spawn() {
                Ok(child) => {
                    if attempts > 1 {
                        tracing::debug!(
                            attempts,
                            max_attempts = MAX_SPAWN_ATTEMPTS,
                            "Captured process spawn succeeded after ETXTBSY retry"
                        );
                    }
                    break child;
                }
                Err(error) if error.raw_os_error() == Some(26) && attempts < MAX_SPAWN_ATTEMPTS => {
                    tracing::warn!(
                        attempt = attempts,
                        max_attempts = MAX_SPAWN_ATTEMPTS,
                        "Retrying captured process spawn after ETXTBSY"
                    );
                    tokio::time::sleep(ETXTBSY_BACKOFF).await;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to spawn captured process at {}",
                            request.program.display()
                        )
                    });
                }
            }
        };
        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .with_context(|| {
                format!(
                    "captured process at {} timed out after {}ms",
                    request.program.display(),
                    timeout.as_millis()
                )
            })?
            .with_context(|| {
                format!(
                    "failed waiting for captured process at {}",
                    request.program.display()
                )
            })?;

        Ok(ProcessOutput {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// In-memory runner for policy tests.
///
/// Responses are consumed in FIFO order and every request is retained for
/// structural assertions. Neither operation starts an operating-system child.
#[derive(Default)]
pub struct FakeProcessRunner {
    responses: Mutex<VecDeque<Result<ProcessOutput, String>>>,
    requests: Mutex<Vec<ProcessRequest>>,
}

impl FakeProcessRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_output(&self, output: ProcessOutput) {
        self.responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back(Ok(output));
    }

    pub fn push_error(&self, message: impl Into<String>) {
        self.responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back(Err(message.into()));
    }

    pub fn requests(&self) -> Vec<ProcessRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[async_trait]
impl ProcessRunner for FakeProcessRunner {
    async fn output(&self, request: ProcessRequest, _timeout: Duration) -> Result<ProcessOutput> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request);

        match self
            .responses
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
        {
            Some(Ok(output)) => Ok(output),
            Some(Err(message)) => bail!(message),
            None => bail!("fake process runner has no queued response"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn process_runner_fake_returns_output_without_spawning() {
        let runner = FakeProcessRunner::new();
        runner.push_output(ProcessOutput::success(b"ready\n".to_vec()));

        let output = runner
            .output(
                ProcessRequest::new("not-a-real-binary").args(["ready", "--json"]),
                Duration::from_secs(30),
            )
            .await
            .unwrap();

        assert!(output.success);
        assert_eq!(output.stdout, b"ready\n");
        let requests = runner.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].program(), Path::new("not-a-real-binary"));
        assert_eq!(requests[0].arguments(), ["ready", "--json"]);
    }

    #[tokio::test]
    async fn process_runner_fake_preserves_failure_outcomes() {
        let runner = FakeProcessRunner::new();
        runner.push_output(ProcessOutput::failure(Some(4), b"conflict".to_vec()));

        let output = runner
            .output(ProcessRequest::new("bead"), Duration::from_secs(30))
            .await
            .unwrap();

        assert!(!output.success);
        assert_eq!(output.exit_code, Some(4));
        assert_eq!(output.stderr, b"conflict");
    }

    #[tokio::test]
    async fn process_runner_fake_surfaces_queued_errors() {
        let runner = FakeProcessRunner::new();
        runner.push_error("synthetic spawn failure");

        let error = runner
            .output(ProcessRequest::new("bead"), Duration::from_secs(30))
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "synthetic spawn failure");
    }
}
