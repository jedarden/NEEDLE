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

# Create three sequential beads
echo "🧶 Creating bead 1: Add CONTRIBUTING.md"
contributing_id=$(bead create --title "Add CONTRIBUTING.md" --priority 2 --issue-type task)
echo "   Created: $contributing_id"

echo "🧶 Creating bead 2: Add LICENSE file"
license_id=$(bead create --title "Add LICENSE file" --priority 2 --issue-type task)
echo "   Created: $license_id"

echo "🧶 Creating bead 3: Add simple Makefile"
makefile_id=$(bead create --title "Add simple Makefile" --priority 1 --issue-type task)
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
