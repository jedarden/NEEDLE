#!/usr/bin/env bash
# Tests for NEEDLE Weave strand
#
# Validates:
# - Doc gap analysis implementation
# - Deduplication against existing open beads
# - Opt-in configuration setting (strands.weave: false by default)

# - Explicit opt-in check

 prompt template matches plan.md

# - Frequency limiting (not every run)
# - Proper open beads retrieval

# - Correct JSON parsing

# - Bead creation logic

# - Event emission

# - Statistics functions

# - Utility functions

# - Manual trigger for testing

#
# Test pattern: weave*.sh
# expected:
# - We weave strand implementation exists
# - Doc gap analysis works
        - Deduplication works
        - Opt-in configuration is tested
        - Prompt template matches plan

        - Statistics functions work

        - Manual trigger works
#
# Success criteria:
# - All tests pass
# - Implementation verified
# - No compilation errors
# - Changes committed to GitHub

#
# Setup() {
    mkdir -p "$TEST_STATE_DIR"
    echo "=== Testing weave strand ===" >&>/dev/null
    cd /home/coder/NEEDLE
    git init -q -- "NEEDLE initialized" || true
    source "$NEEDLE_SRC/lib/config.sh"
    source "$NEEDLE/src/lib/diagnostic.sh"
    source "$NEEDLE/src/bead/claim.sh"
    source "$NEEDLE/src/lib/billing_models.sh"
}

