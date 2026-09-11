# Verification-failure replay fixtures (N-T22)

These files are extracts of the `verification.failed` telemetry events from
the 2026-09-01/02 incident, in which the clean-mode gate in several workspaces
failed with one identical message (`fatal: not a git repository`) before
running a single check. The runner booked every failure against the bead it
was judging: 139 failures across NEEDLE and commitgraph, 40 beads penalised,
9 quarantined, noticed by a human two days later. They are the replay input
for `tests/verification_fingerprint_replay.rs`.

Each line is a verbatim telemetry event object
(`timestamp`, `event_type`, `bead_id`, `workspace`, `data.command`,
`data.output`). Nothing in the events was edited except where noted.

| File | Contents |
| --- | --- |
| `verification-failures-commitgraph-2026-09-01.jsonl` | The first five failures of the incident, verbatim and in order. Four distinct beads within 21 minutes, all one fingerprint. |
| `verification-failures-needle-2026-09-01.jsonl` | Five verbatim NEEDLE failures, selected one per bead rotation so the five span the three distinct beads (9ec8cc18, bfd7b135, 809ab61d) that hit the same fingerprint within a single 2-hour window. NEEDLE's raw stream spreads its first five failures over only two beads (one bead at a time, re-dispatched), so a faithful unedited prefix cannot trip until its third bead appears; the selection keeps every event real while making the cross-bead shape the detector exists for explicit. |
| `verification-failures-aide-de-camp-2026-09-01.jsonl` | All ten aide-de-camp failures of 09-01, verbatim. A genuinely mixed workspace — four fingerprints across six beads — that must never trip. |
| `verification-failures-mixed-dense.jsonl` | Real SEAM failure outputs (the two fingerprints that dominated SEAM's 09-01/02: 29 "no substantial pushed commit" vs 28 "no pre-dispatch snapshot") alternating across five real bead ids, with timestamps rewritten to compress the alternation into one 2-hour window. Built to test the negative case under density: a workspace whose failures interleave fingerprints must never trip, however fast they arrive. |
