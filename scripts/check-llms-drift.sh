#!/usr/bin/env bash
# check-llms-drift.sh — Ensure llms.txt commands match README.md Quickstart verbatim
#
# This script validates that every executable command line in llms.txt appears
# exactly in README.md's Quickstart section, preventing documentation drift.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LLMS_TXT="$REPO_ROOT/llms.txt"
README_MD="$REPO_ROOT/README.md"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
NC='\033[0m' # No Color

echo "Checking llms.txt → README.md drift..."

# Extract executable commands from llms.txt
# - Skip comment lines (starting with #)
# - Skip empty lines
# - Skip section headers (lines with only words and spaces)
# - Capture only actual shell commands
mapfile -t llms_commands < <(
  grep -E '^\s*(curl|cd|bead|needle|tmux|git)' "$LLMS_TXT" 2>/dev/null || true
)

if [ ${#llms_commands[@]} -eq 0 ]; then
  echo -e "${RED}✗ No commands found in llms.txt${NC}"
  exit 1
fi

echo -e "${GREEN}Found ${#llms_commands[@]} commands in llms.txt${NC}"

# Track missing commands
missing_commands=0
for cmd in "${llms_commands[@]}"; do
  # Strip leading whitespace for comparison
  cmd_clean=$(echo "$cmd" | sed 's/^\s*//')

  # Check if this command exists in README.md
  if ! grep -Fq "$cmd_clean" "$README_MD"; then
    echo -e "${RED}✗ Command not found in README.md:${NC} $cmd_clean"
    ((missing_commands++))
  fi
done

# Check that README.md has the quickstart section at all
if ! grep -q '## 🚀 Quickstart' "$README_MD"; then
  echo -e "${RED}✗ README.md missing 'Quickstart' section${NC}"
  exit 1
fi

# Report results
if [ $missing_commands -gt 0 ]; then
  echo ""
  echo -e "${RED}✗ Drift check failed: $missing_commands commands not found in README.md${NC}"
  echo ""
  echo "To fix:"
  echo "1. Ensure every command in llms.txt appears verbatim in README.md Quickstart"
  echo "2. Run this script again to verify"
  exit 1
else
  echo -e "${GREEN}✓ All ${#llms_commands[@]} commands present in README.md${NC}"
  echo ""
  echo "Commands verified:"
  for cmd in "${llms_commands[@]}"; do
    cmd_clean=$(echo "$cmd" | sed 's/^\s*//')
    echo -e "  ${GREEN}✓${NC} $cmd_clean"
  done
  echo ""
  echo -e "${GREEN}✓ Drift check passed${NC}"
  exit 0
fi
