use std::path::Path;

fn main() {
    println!("Testing telemetry FileSink creation...");

    let home = std::env::var("HOME").unwrap();
    let log_dir = std::path::PathBuf::from(&home).join(".needle").join("logs");

    let worker_id = "test-worker-debug";
    let session_id = "test1234";

    println!("Log dir: {:?}", log_dir);
    println!("Worker ID: {}", worker_id);
    println!("Session ID: {}", session_id);

    // Create directory
    std::fs::create_dir_all(&log_dir).expect("create dir failed");

    let filename = format!("{}-{}.jsonl", worker_id, session_id);
    let path = log_dir.join(filename);

    println!("Path: {:?}", path);

    // Open file
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .expect("open failed");

    println!("File opened: {:?}", file);

    // Write to file
    use std::io::Write;
    let mut writer = std::io::BufWriter::new(file);
    writeln!(writer, "{{\"test\": \"data\"}}").expect("write failed");
    writer.flush().expect("flush failed");

    println!("Wrote and flushed");

    // Check file size
    let metadata = std::fs::metadata(&path).expect("metadata failed");
    println!("File size: {} bytes", metadata.len());

    // Read back
    let content = std::fs::read_to_string(&path).expect("read failed");
    println!("Content: {}", content);

    // Clean up
    std::fs::remove_file(&path).ok();
    println!("Cleaned up");
}
