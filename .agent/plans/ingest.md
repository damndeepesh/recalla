# Plan: ingest pipeline + `recalla backfill`

## Goal

`recalla backfill` imports every Claude Code session, both the existing history and anything new since the last run, into the store. It is resumable, idempotent and redacted. Each event is stored redacted and truncated, with exact provenance, plus an immutable compressed archive of the raw bytes. Extraction into atomic memories is the NEXT TODO item. Storing events now means the extractor can be re-run as it improves, without reparsing logs.

## Design

- **Data dir:** `$RECALLA_HOME`, else `$XDG_DATA_HOME/recalla`, else `~/.local/share/recalla`. Created 0700, DB file 0600.
  - `recalla.db`: SQLite store.
  - `archive/`: zstd segments.
- **Migration v2** (append-only; v1 shipped on main):
  - `sources(id, harness, path UNIQUE, identity, size, next_offset, events, updated_at)`: one checkpoint per log file.
  - `events(id, source_id, harness, session_id, message_uuid, parent_uuid, line, byte_offset, ts, cwd, git_branch, sidechain, kind, tool_name, tool_id, is_error, text)`.
    - `UNIQUE(source_id, line)`, so a re-run is a no-op.
    - Plus `events_fts` (FTS5 over text) for `why`/search.
  - Provenance unique index rebuilt to include `source_path`. Subagent files share the parent `sessionId`.
- **File identity:** blake3 of the first 4 KB plus the file length.
  - Same identity prefix, and length ≥ `next_offset`: resume from `next_offset` (append-only logs).
  - Prefix changed or file shrank: re-parse from 0. `UNIQUE(source_id, line)` plus a delete-and-reinsert of that source's events keeps it consistent.
- **Per file:**
  1. `parse_session(path, offset)`
  2. For each event: `redact(text)`, then `truncate_middle(MAX_EVENT_BYTES)`
  3. Archive the raw bytes `[offset, next_offset)` as one zstd segment
  4. In ONE `IMMEDIATE` transaction: insert events, update the checkpoint

  A crash mid-file loses at most that file's new tail, which is re-read next run.

- **Archive:** `archive/<blake3-of-segment>.zst`, content-addressed and write-once (written to a temp file, then renamed).
  - The `segments(source_id, start, end, hash)` table can reproduce the original byte-for-byte by concatenation.
  - The archive holds RAW (unredacted) bytes. It is a local copy of data already on disk, stored 0600.
  - `--no-archive` turns it off. The default is on, per SPEC.
- **Report:** files seen / new / resumed / restarted / unchanged, events added, redactions by rule, archive bytes (raw → compressed), duration. Human output by default, `--json` for scripts. Counts only, never content.
- **CLI:** `clap` derive.
  - `recalla backfill [--root PATH] [--no-archive] [--dry-run] [--json]`
  - `recalla --version`
  - `main.rs` stays thin; logic lives in `ingest`.
- **Errors:** a broken file is reported and skipped; the run continues and the exit code is 1 if any file failed. No silent catches.

## Steps

- [x] 1. Deps `clap` (derive) and `zstd`, with D-013 (CLI parser) and D-014 (archive compression)
- [x] 2. `paths` module: data-dir resolution (pure fn + env wrapper), create dirs with 0700/0600 perms
- [x] 3. Store migration v2: `sources`, `events`, `events_fts` + triggers, `segments`, rebuilt provenance index; migration test from a v1 DB
- [x] 4. Store APIs: `source_checkpoint(path)`, `ingest_batch(source, events, checkpoint)` (one txn), `reset_source(id)`, event counts
- [x] 5. `archive` module: `write_segment(bytes) -> hash` (atomic, idempotent), `read_source(store, source_id) -> Vec<u8>`; round-trip test
- [x] 6. `ingest` module: per-file pipeline, identity check, redact then truncate, `Report`
- [x] 7. CLI `backfill` + `--version`, human/JSON output, exit codes
- [x] 8. Integration tests over a fixture tree copied to a tempdir:
  - first run imports everything
  - second run imports 0
  - an appended line is resumed
  - a rewritten file is restarted
  - a runtime-built secret in a fixture never reaches the DB (scan every DB text column)
  - the archive reproduces the source byte-for-byte
  - `--no-archive` writes nothing
  - `--dry-run` writes nothing
- [x] 9. Real-data smoke into a temp `RECALLA_HOME` (counts and timing only)
- [x] 10. Docs: ARCHITECTURE (ingest, schema v2), AGENTS commands, README status, TODO, CHANGELOG, state

## Files touched

- `Cargo.toml`/`Cargo.lock`
- `src/main.rs`, `src/lib.rs`, `src/cli.rs`, `src/paths.rs`, `src/archive.rs`, `src/ingest.rs`
- `src/store/{mod.rs,schema.rs}` (+ `events.rs`)
- `tests/ingest.rs`
- docs

## Reused modules

`harness::claude::{discover, parse_session}`, `harness::truncate_middle`, `redact::redact`, `store::Store`, and the blake3 already in deps.

## Risks

- **Secrets reaching disk:** every text field passes `redact` before insert; a test scans all DB text after ingesting a secret-bearing fixture. The archive is raw by design: documented, 0600, opt-out.
- **Half-written files** (a live session): the harness never consumes a partial last line, so the next run picks it up.
- **Log rotation or edits:** the identity check falls back to a full reparse; per-source delete-and-reinsert keeps it consistent.
- **DB growth:** about 50% of raw log size. Measured in step 9.
- **Concurrency** (a hook capture while a backfill runs): short per-file IMMEDIATE transactions + `busy_timeout`.

## Test plan

- Unit: data-dir resolution, identity check, archive atomic write.
- Integration: the step-8 list.
- Migration: open a v1 DB, migrate to v2, data preserved.
- Smoke on real logs.
- `make check` + `make secrets` green.
