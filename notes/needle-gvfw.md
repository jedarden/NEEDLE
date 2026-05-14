# needle-gvfw: Transcript Discovery and Parsing

## Summary

The transcript discovery and parsing implementation was completed across prior
sessions (commits `fcb2c32`, `b90d181`, `afb9927`, `d7dc332`).

## What was implemented

### `src/transcript/mod.rs` (1,316 lines)

- **`TranscriptDiscovery`** struct that locates session JSONL files under
  `~/.claude/projects/<project-name>/` by deriving the project name from the
  workspace path (`/home/coding/NEEDLE` → `-home-coding-NEEDLE`).
- **Recency filtering** via `with_recency_cutoff()` (time-based) and
  `max_sessions` limit (count-based).
- **`parse_transcript()`** reads each JSONL file line-by-line with graceful
  handling of malformed or incomplete lines (logged at `TRACE`, not errors).
- **`ParsedTranscript`** structured type capturing: session ID, modification
  time, task description, actions, action-outcome pairs, bead ID, and workspace.
- **`ActionOutcome`** pairs `tool_use` ↔ `tool_result` entries with
  human-readable summaries and `to_pattern()` for learning extraction.
- **`discover_workspaces()`** for multi-workspace discovery with per-workspace
  and global caps, sorted by recency.
- **Decision detection** — analyzes thinking blocks and assistant text for
  ADR-style decision patterns with confidence scoring.
- **14 unit tests** covering discovery, parsing, recency filtering, tool input
  formatting, bead ID extraction, and multi-workspace scenarios.

### Integration

- `src/strand/reflect.rs` imports `ParsedTranscript` and `TranscriptDiscovery`
  and uses them during the Gather phase.
- `config::ReflectConfig` has `transcript_recency_days` (default 7) and
  `transcript_max_sessions` (default 50) fields wired up.
- `src/drift/mod.rs` uses `SessionFingerprint::from_transcript()` for drift
  detection across sessions.

## Acceptance criteria verification

- **Enumerate recent sessions**: `TranscriptDiscovery::discover()` lists JSONL
  files from the project directory, sorted by mtime, capped at `max_sessions`.
- **Structured representation**: `ParsedTranscript` with `actions` (Vec of
  `TranscriptAction`) and `action_outcomes` (Vec of `ActionOutcome`).
- **Graceful malformed file handling**: parse errors on individual JSONL lines
  are logged at `TRACE` and skipped; the transcript is still returned with
  whatever was successfully parsed.
