.PHONY: test test-fast test-slow test-install fmt clippy check clean

# Default test target - runs fast checks
test: test-fast

# Fast lane checks (fmt, clippy, check)
test-fast:
	@echo "Running fast lane checks..."
	@bash scripts/definition-of-done.sh --fast

# Slow lane checks (unit, integration, and installer tests)
test-slow:
	@echo "Running slow lane checks..."
	@bash scripts/definition-of-done.sh --slow

# Run all tests (fast + slow)
test-all:
	@echo "Running all tests..."
	@bash scripts/definition-of-done.sh --all

# Run installer tests only
test-install:
	@echo "Running installer tests..."
	@bash tests/installer/run.sh

# Format code
fmt:
	@echo "Formatting code..."
	@cargo fmt

# Check code formatting
fmt-check:
	@echo "Checking code formatting..."
	@cargo fmt -- --check

# Run clippy
clippy:
	@echo "Running clippy..."
	@cargo clippy --all-targets -- -D warnings

# Run cargo check
check:
	@echo "Running cargo check..."
	@cargo check

# Clean build artifacts
clean:
	@echo "Cleaning build artifacts..."
	@cargo clean
	@rm -rf .beads/checkpoint/objects/*.jsonl

# Help target
help:
	@echo "NEEDLE Test Targets"
	@echo ""
	@echo "  make test          - Run fast lane checks (default)"
	@echo "  make test-fast     - Run fast lane checks (fmt, clippy, check)"
	@echo "  make test-slow     - Run slow lane checks (all tests)"
	@echo "  make test-all      - Run all tests (fast + slow)"
	@echo "  make test-install  - Run installer tests only"
	@echo ""
	@echo "  make fmt           - Format code with cargo fmt"
	@echo "  make fmt-check     - Check code formatting"
	@echo "  make clippy        - Run clippy linter"
	@echo "  make check         - Run cargo check"
	@echo "  make clean         - Clean build artifacts"
	@echo ""
	@echo "Installer Tests:"
	@echo "  Isolated shell-level regression tests for install.sh"
	@echo "  Uses local fixtures and mocks - no real network calls"
	@echo "  Parallel-safe and does not touch real user installations"
	@echo ""
	@echo "  Coverage includes:"
	@echo "    - Missing checksums"
	@echo "    - Mismatched checksums"
	@echo "    - Valid checksums"
	@echo "    - Missing SHA-256 tool"
	@echo "    - Opt-out flag usage (--skip-checksum, NEEDLE_SKIP_CHECKSUM)"
