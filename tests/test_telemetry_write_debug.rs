// Test program to verify telemetry writes correctly
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Testing telemetry write...");

    // Create a minimal test
    let log_dir = PathBuf::from("/tmp/needle-test-logs");
    std::fs::create_dir_all(&log_dir)?;

    // Simulate what Telemetry::new() does
    let worker_id = "test-worker";
    let session_id = "test1234";

    // Create file
    let filename = format!("{}-{}.jsonl", worker_id, session_id);
    let path = log_dir.join(&filename);

    println!("Creating file: {:?}", path);

    use std::io::Write;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;

    let mut writer = std::io::BufWriter::new(file);

    // Write a test line
    let test_line = r#"{"test": "data"}"#;
    writeln!(writer, "{}", test_line)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;

    drop(writer);

    // Check file size
    let metadata = std::fs::metadata(&path)?;
    println!("File size: {} bytes", metadata.len());

    if metadata.len() > 0 {
        println!("SUCCESS: File has content");
        let content = std::fs::read_to_string(&path)?;
        println!("Content: {}", content);
    } else {
        println!("FAIL: File is empty");
    }

    Ok(())
}
