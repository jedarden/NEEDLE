# Test Output Capture

The `test_output` module provides utilities for managing test output files including stdout, stderr, and combined output.

## Overview

Test outputs are stored in a dedicated `.test_outputs/` directory structure with proper error handling for directory creation failures.

## Directory Structure

```
.test_outputs/
└── <test-name>/
    ├── stdout.txt      # Raw stdout from test execution
    ├── stderr.txt      # Raw stderr from test execution
    └── combined.txt    # Combined stdout + stderr with interleaving
```

## Usage

### Basic Usage

```rust
use needle::test_output::TestOutput;
use std::path::Path;

// Create test output directory structure
let output = TestOutput::new("my_test", Path::new(".")).unwrap();

// Write test outputs
output.write_stdout("Test stdout content").unwrap();
output.write_stderr("Test stderr content").unwrap();
output.write_combined("Combined output").unwrap();

// Read test outputs
let stdout = output.read_stdout().unwrap();
let stderr = output.read_stderr().unwrap();
let combined = output.read_combined().unwrap();
```

### Utility Functions

```rust
use needle::test_output::{test_output_dir, ensure_test_output_dir, cleanup_all_test_outputs};
use std::path::Path;

// Get the global test output directory path
let dir = test_output_dir(Path::new("."));

// Ensure the global test output directory exists
ensure_test_output_dir(Path::new(".")).unwrap();

// Clean up all test outputs (use with caution!)
cleanup_all_test_outputs(Path::new(".")).unwrap();
```

## Error Handling

The module provides comprehensive error handling:

- **Directory creation failures**: Returns `None` from `TestOutput::new()` if directory creation fails
- **File write failures**: Returns `Result::Err` with detailed context if file operations fail
- **File read failures**: Returns `Result::Err` with detailed context if files don't exist or can't be read

## File Path Constants

The module provides constants for the file names:

- `STDOUT_FILE`: "stdout.txt"
- `STDERR_FILE`: "stderr.txt" 
- `COMBINED_FILE`: "combined.txt"
- `TEST_OUTPUT_DIR_NAME`: ".test_outputs"

## Testing

The module includes comprehensive tests covering:

- Directory creation
- File writing and reading
- Error handling for directory creation failures
- Cleanup operations
- File path operations

Run tests with:

```bash
cargo test --lib test_output
```

## Integration with CI

The `.test_outputs/` directory is included in `.gitignore`, so test outputs won't be committed to the repository. This makes it suitable for CI/CD environments where test output capture is needed but persistence is not required.