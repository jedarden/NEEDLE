#!/usr/bin/bash
# Seed beads for the NEEDLE quickstart example
# Creates three beads with one dependency to demonstrate the ready frontier

set -e

echo "📋 Creating three seed beads for quickstart example..."

# Check if bead store is initialized
if [ ! -d .beads ]; then
  echo "❌ Bead store not initialized. Run 'bead init --prefix quickstart' first."
  exit 1
fi

# Create three sequential beads. Each one names a single deliverable and its
# acceptance command, and its description prevents unnecessary decomposition.
echo "🧶 Creating bead 1: Add CONTRIBUTING.md"
contributing_id=$(bead create \
  --title 'Create CONTRIBUTING.md so that test -s CONTRIBUTING.md passes. Work on this issue directly.' \
  --description 'Create CONTRIBUTING.md with contribution guidelines. Acceptance: `test -s CONTRIBUTING.md`. Do not create sub-issues, split this work, or decompose it.' \
  --priority 2 --issue-type task)
echo "   Created: $contributing_id"

echo "🧶 Creating bead 2: Add LICENSE file"
license_id=$(bead create \
  --title 'Create LICENSE so that test -s LICENSE passes. Work on this issue directly.' \
  --description 'Create LICENSE with the project license text. Acceptance: `test -s LICENSE`. Do not create sub-issues, split this work, or decompose it.' \
  --priority 2 --issue-type task)
echo "   Created: $license_id"

echo "🧶 Creating bead 3: Add simple Makefile"
makefile_id=$(bead create \
  --title 'Create Makefile so that test -s Makefile passes. Work on this issue directly.' \
  --description 'Create a simple Makefile with the project command. Acceptance: `test -s Makefile`. Do not create sub-issues, split this work, or decompose it.' \
  --priority 1 --issue-type task)
echo "   Created: $makefile_id"

# Add a dependency: Makefile depends on LICENSE
echo "🔗 Adding dependency: $makefile_id depends on $license_id"
bead dep add "$makefile_id" "$license_id"

echo ""
echo "✅ Bead seeding complete!"
echo ""
echo "📊 Current bead state:"
bead list --status open

echo ""
echo "🎯 Ready frontier (claimable now):"
bead list --ready

echo ""
echo "💡 Run 'needle run --agent claude -i alpha' to start processing"
