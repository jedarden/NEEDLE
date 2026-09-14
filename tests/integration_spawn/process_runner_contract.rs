//! Real-process contract for the production ProcessRunner adapter.

use needle::process_runner::{ProcessRequest, ProcessRunner, TokioProcessRunner};
use std::time::Duration;

#[cfg(unix)]
#[tokio::test]
async fn tokio_process_runner_captures_status_streams_environment_and_working_directory() {
    let directory = tempfile::tempdir().unwrap();
    let runner = TokioProcessRunner;
    let request = ProcessRequest::new("/bin/sh")
        .args([
            "-c",
            "printf '%s|%s' \"$RUNNER_CONTRACT\" \"$PWD\"; printf 'contract-stderr' >&2; exit 7",
        ])
        .current_dir(directory.path())
        .env("RUNNER_CONTRACT", "contract-stdout");

    let output = runner
        .output(request, Duration::from_secs(5))
        .await
        .unwrap();

    assert!(!output.success);
    assert_eq!(output.exit_code, Some(7));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("contract-stdout|{}", directory.path().display())
    );
    assert_eq!(output.stderr, b"contract-stderr");
}
