# Execution checkpoint persistence (C1)

`needle_learning::ExecutionCheckpoint` is a version 1 observation tied to one
bead, attempt, and nonzero claim epoch. Its bounded fields record the intended
result, observable result, optional concise rationale, next intervention, and
up to 16 evidence references. `CheckpointEvidence::UncommittedRecovery` carries
an explicit `Owned` or `Ambiguous` attribution; `VerifiedArtifact` carries an
immutable artifact digest. Neither type contains raw transcript or tool output.
The timestamp must be RFC 3339, and the serialized record is capped at 8 KiB.

`ExecutionCheckpointStore::append` is the runtime write entry point. Construct
it with the target workspace, bead ID, and attempt ID, then pass the record,
the held `ClaimIdentity`, the target `BeadStore`, and the configured `Sanitizer`.
The method reads the current claim from that store, requires the exact active
assignee, revision, and epoch, and rejects any field the sanitizer would change.
Unsupported versions, invalid IDs, oversized text, sensitive fields, claim
read failures, and stale ownership are errors. Producers must handle these
errors explicitly; a rejected observation is not evidence.

Accepted records live under the existing
`.beads/traces/<bead-id>/<attempt-id>/` directory as
`execution-checkpoint-<claim-epoch>-<checkpoint-id>.json`. Publishing writes
and syncs a private temporary file, then creates the final name without
replacement and syncs the directory. A replay of the same record returns
`AlreadyPresent` without writing; the same key with different content fails.
`read_all` uses the same sanitizer, ignores unfinished temporary files after a
crash, and fails on a malformed or sensitive published file. Attempt trace
layout, bundle inclusion, upload, and
retention remain owned by the existing trace/archive work. This API does not
activate any lifecycle producer or change the resolution gate.

The claim read and local file publication are separate operations. The final
attempt resolution still needs its fenced backend operation; local checkpoint
persistence alone grants no ownership or verified outcome.
