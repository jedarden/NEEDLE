use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::time::{timeout, Duration};

#[tokio::test]
async fn test_echo_agent_simple() {
    // Simplified test to isolate the hanging issue
    let invoke_template = "echo done";
    let workspace = PathBuf::from(".");
    let prompt_file = PathBuf::from("/tmp/test_prompt.txt");

    // Write a simple prompt
    std::fs::write(&prompt_file, "test prompt").unwrap();

    // Render the template (no variables in this case)
    let rendered = invoke_template;

    println!("Template rendered: {:?}", rendered);

    // Spawn the process like the dispatcher does
    let mut child = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(&rendered)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    let pid = child.id().unwrap_or(0);
    println!("Spawned process with PID: {}", pid);

    // Wait for completion with timeout
    let result = timeout(Duration::from_secs(5), child.wait()).await;

    match result {
        Ok(Ok(status)) => {
            println!("Process exited with status: {:?}", status);
            assert!(status.success(), "echo should succeed");
        }
        Ok(Err(e)) => {
            panic!("Timeout waiting for process: {}", e);
        }
        Err(e) => {
            panic!("Join error: {}", e);
        }
    }
}
