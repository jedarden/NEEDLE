# Fixture guidance

Deduplicate the status formatting implementation across the two callers.
Read `.canary/bead.md`, run `.canary/gate.sh`, and close the local fixture
bead only after the gate passes. Keep the public `format_status` behavior.
