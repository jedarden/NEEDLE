# Compilation Error Detection and Test Execution Flow

This document describes the full workflow for detecting, recording, and analyzing compilation errors in the NEEDLE bead worker system.

## Overview

The compilation error detection system integrates deeply with the cargo test execution pipeline to provide comprehensive error classification and trace capture. This enables post-mortem analysis of failed test runs and distinguishes between compilation failures and runtime test failures.

## Architecture

### Components

1. **CompilationError** - Detailed error structure with code, variant, message, and location
2. **CompilationErrorVariant** - Enum classifying errors by type (TypeMismatch, BorrowChecker, etc.)
3. **detect_compilation_errors()** - Parses stderr to extract errors
4. **TraceCapture::write_compilation_errors()** - Writes errors to trace directory
5. **CargoTest::run_with_bead_trace()** - Orchestrates the full workflow

### Data Flow

```
cargo test execution
    ↓
stdout/stderr capture
    ↓
detect_compilation_errors(stderr)
    ↓
TestOutcome { compilation_failed, compilation_errors }
    ↓
run_with_bead_trace()
    ↓
TraceCapture writes:
    - stdout.txt
    - stderr.txt
    - test_metrics.json
    - compilation_errors.json (if errors detected)
```

## Error Classification

### Supported Error Variants

| Variant | Description | Example Error Codes |
|--------|-------------|---------------------|
| TypeMismatch | Type incompatibility errors | E0308, E0309, E0061, E0063 |
| BorrowChecker | Ownership/lifetime violations | E0382, E0502, E0505, E0506 |
| ImportOrPath | Module/import resolution failures | E0432, E0433, E0583, E0603 |
| Lifetime | Lifetime annotation errors | E0495, E0597, E0623 |
| TraitBound | Trait constraint violations | E0277, E0207, E0119 |
| Syntax | Parse/syntax errors | (no error code) |
| Unused | Dead code warnings treated as errors | unused_variables, dead_code |
| Other | Unclassified compiler errors | - |
| Unknown | Error codes not in classification | - |

## Full Test Execution Flow

### 1. Test Execution Initiation

```rust
let runner = CargoTest::new(workspace_path);
let bead_id = "bf-12345";

// Execute with trace capture
let outcome = runner.run_with_bead_trace(bead_id)?;
```

### 2. Process Spawning and Output Capture

The system spawns a cargo test process with:
- Configured timeout (default: 600 seconds)
- Piped stdout and stderr for capture
- Thread-based monitoring for timeout enforcement

```rust
let mut cmd = Command::new("cargo");
cmd.args(&args);
cmd.current_dir(&workspace);
cmd.stdout(Stdio::piped());
cmd.stderr(Stdio::piped());
```

### 3. Output Parsing and Error Detection

After process completion, the system parses stderr:

```rust
let (compilation_failed, compilation_errors) =
    detect_compilation_errors(&String::from_utf8_lossy(&output.stderr));
```

The `detect_compilation_errors()` function identifies:
- `error[E####]:` patterns - Rust compiler errors with codes
- `could not compile` messages - Compilation failure indicators
- `aborting due to N previous errors` - Error count summaries
- `error:` with unused/dead_code - Warnings treated as errors

### 4. Outcome Construction

The `TestOutcome` struct captures:

```rust
pub struct TestOutcome {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
    pub timed_out: bool,
    pub compilation_failed: bool,
    pub compilation_errors: Vec<CompilationError>,
}
```

### 5. Trace Directory Creation

The system creates the trace directory structure:

```
.beads/traces/<bead-id>/
├── stdout.txt
├── stderr.txt
├── test_metrics.json
└── compilation_errors.json (if errors present)
```

### 6. Output File Writing

For each output type, the system:

```rust
// Write stdout
trace.write_stdout(&outcome.stdout)?;

// Write stderr
trace.write_stderr(&outcome.stderr)?;

// Write test metrics
let metrics = outcome.to_metrics(test_name);
trace.write_test_metrics(&metrics)?;

// Write compilation errors (if present)
if !outcome.compilation_errors.is_empty() {
    trace.write_compilation_errors(&outcome.compilation_errors)?;
}
```

### 7. Error Classification and Serialization

Each compilation error is classified:

```rust
impl CompilationError {
    pub fn new(
        code: Option<String>,
        message: String,
        file: Option<String>,
        line: Option<usize>,
        column: Option<usize>,
    ) -> Self {
        let variant = if let Some(ref code) = code {
            CompilationErrorVariant::from_code(code)
        } else {
            CompilationErrorVariant::Syntax
        };
        // ...
    }
}
```

The error is then serialized to JSON:

```json
[
  {
    "code": "E0308",
    "variant": "TypeMismatch",
    "message": "mismatched types",
    "file": "src/main.rs",
    "line": 10,
    "column": 5
  }
]
```

### 8. Outcome Determination

The system classifies the overall outcome:

```rust
pub fn is_compilation_failure(&self) -> bool {
    self.compilation_failed
}

pub fn is_test_failure(&self) -> bool {
    !self.success() && !self.timed_out && !self.compilation_failed
}

pub fn success(&self) -> bool {
    self.exit_code == Some(0) && !self.timed_out
}
```

## Trace Output Structure

### stdout.txt

Raw stdout from cargo test execution, including:
- Compilation progress messages
- Test execution output
- Test results

### stderr.txt

Raw stderr from cargo test execution, including:
- Compiler error messages
- Warning messages
- Test failure output

### test_metrics.json

Structured test execution metrics:

```json
{
  "test_name": "cargo_test_bf-12345",
  "exit_code": 101,
  "duration_ms": 1234,
  "timed_out": false,
  "stdout_len": 1024,
  "stderr_len": 2048,
  "timestamp": "2026-07-14T12:34:56.789Z"
}
```

### compilation_errors.json

Detailed compilation error information (only present when compilation fails):

```json
[
  {
    "code": "E0308",
    "variant": "TypeMismatch",
    "message": "mismatched types: expected `i32`, found `&str`",
    "file": "src/main.rs",
    "line": 10,
    "column": 5
  },
  {
    "code": "E0382",
    "variant": "BorrowChecker",
    "message": "use of moved value",
    "file": "src/main.rs",
    "line": 15,
    "column": 8
  }
]
```

## Example Workflow

### Successful Compilation

```rust
// Test code compiles and passes
let runner = CargoTest::new(workspace);
let outcome = runner.run_with_bead_trace("bf-success")?;

assert!(outcome.success());
assert!(!outcome.is_compilation_failure());
assert!(outcome.compilation_errors.is_empty());

// Trace directory contains:
// - stdout.txt (test output)
// - stderr.txt (warnings, if any)
// - test_metrics.json (exit_code: 0)
// compilation_errors.json (NOT created)
```

### Compilation Failure

```rust
// Test code has type mismatch
let runner = CargoTest::new(workspace);
let outcome = runner.run_with_bead_trace("bf-compile-fail")?;

assert!(!outcome.success());
assert!(outcome.is_compilation_failure());
assert!(!outcome.compilation_errors.is_empty());

// Verify specific error
let type_errors: Vec<_> = outcome.compilation_errors
    .iter()
    .filter(|e| e.code.as_deref() == Some("E0308"))
    .collect();
assert!(!type_errors.is_empty());

// Trace directory contains:
// - stdout.txt (compilation progress)
// - stderr.txt (compiler errors)
// - test_metrics.json (exit_code: 101)
// - compilation_errors.json (E0308 error details)
```

### Test Failure (No Compilation Errors)

```rust
// Test code compiles but test fails
let runner = CargoTest::new(workspace);
let outcome = runner.run_with_bead_trace("bf-test-fail")?;

assert!(!outcome.success());
assert!(!outcome.is_compilation_failure());
assert!(outcome.is_test_failure());
assert!(outcome.compilation_errors.is_empty());

// Trace directory contains:
// - stdout.txt (test execution output)
// - stderr.txt (test failure messages)
// - test_metrics.json (exit_code: 1)
// compilation_errors.json (NOT created)
```

## Error Detection Patterns

### Type Mismatch (E0308)

```
error[E0308]: mismatched types
  --> src/main.rs:10:5
   |
10 |     let x: i32 = "hello";
   |            ---   ^^^^^^^ expected `i32`, found `&str`
```

Detected as:
- code: "E0308"
- variant: TypeMismatch
- file: "src/main.rs"
- line: 10
- column: 5

### Borrow Checker (E0382)

```
error[E0382]: use of moved value
  --> src/main.rs:15:8
   |
15 |     let _used = s;
   |                 ^ value moved here in previous iteration
```

Detected as:
- code: "E0382"
- variant: BorrowChecker
- file: "src/main.rs"
- line: 15
- column: 8

### Import Error (E0433)

```
error[E0433]: failed to resolve: use of undeclared crate or module
  --> src/main.rs:5:5
   |
5 | use nonexistent::Module;
   |     ^^^^^^^^^^^^^^^ use of undeclared crate or module
```

Detected as:
- code: "E0433"
- variant: ImportOrPath
- file: "src/main.rs"
- line: 5
- column: 5

## Integration Tests

The system includes comprehensive end-to-end tests:

### Test Coverage

1. **test_end_to_end_compilation_error_workflow**
   - Creates project with multiple compilation errors
   - Verifies error detection and trace file creation
   - Validates JSON serialization of errors

2. **test_end_to_end_successful_compilation_workflow**
   - Verifies clean compilation produces no compilation_errors.json
   - Confirms trace files are created for successful runs

3. **test_end_to_end_test_failure_workflow**
   - Distinguishes test failures from compilation errors
   - Verifies compilation_errors.json is not created for test failures

### Running Tests

```bash
# Run all compilation error detection tests
cargo test --test compilation_error_detection

# Run specific end-to-end test
cargo test test_end_to_end_compilation_error_workflow

# Run with output
cargo test --test compilation_error_detection -- --nocapture
```

## Troubleshooting

### No compilation_errors.json Created

If compilation errors occurred but no JSON file was created:
1. Check that `compilation_failed` is true in the outcome
2. Verify `compilation_errors` vector is not empty
3. Check trace directory creation succeeded
4. Review logs for write errors

### Errors Not Classified

If errors show "Unknown" variant:
1. Verify error code is in the classification list
2. Check error line matches expected pattern
3. Review stderr output format

### Test Failures vs Compilation Errors

If tests are misclassified:
1. Check exit code (101 = compilation, 1 = test failure)
2. Review stderr for "error[E" patterns
3. Verify test code compiles successfully

## Performance Considerations

### Output Truncation

Stdout and stderr are truncated to 64KB to prevent memory issues:

```rust
const MAX_OUTPUT_BYTES: usize = 65536;
```

### Timeout Handling

Tests timeout after 10 minutes by default:

```rust
pub const DEFAULT_TEST_TIMEOUT_SECS: u64 = 600;
```

### Trace File Management

- Failed beads: traces retained for 30 days
- Successful beads: traces retained for 7 days, then pruned
- Trace pruning removes stdout.txt, stderr.txt, keeps metadata.json

## Future Enhancements

Potential improvements:
1. Location parsing (file, line, column) from error context
2. Error suggestion integration (Rust compiler fix-its)
3. Aggregate error statistics across multiple runs
4. Error trend analysis over time
5. Integration with code quality metrics
