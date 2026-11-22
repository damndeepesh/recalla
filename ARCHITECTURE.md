# Architecture

## Overview

A single Rust binary with no daemon. Each command (hook, CLI, or the MCP stdio server) opens the SQLite store, does its work, and exits.

```mermaid
flowchart LR
  S[Agent sessions JSONL] --> I[ingest] --> R[redact] --> X[extract] --> ST[(store: SQLite+FTS5)]
  I --> A[archive zstd]
  ST <--> M[mirror .md]
  C[code repo] --> AN[anchor tree-sitter] --> ST
  ST --> P[pack + ledger] --> H[hooks / CLI / MCP]
```

## Components

| Component | Responsibility                                                                                     | Depends on              |
| --------- | -------------------------------------------------------------------------------------------------- | ----------------------- |
| `harness` | Per-agent adapters: locate sessions, parse JSONL into normalized `Event`s, install/uninstall hooks | none                    |
| `ingest`  | Resumable scan → parse → normalize pipeline with checkpoints (file, byte offset)                   | harness, archive, store |
| `archive` | Immutable zstd copies of source sessions, keyed by content hash                                    | none                    |
| `redact`  | Layered secret scrubbing (rules + entropy) on all text before persist                              | none                    |
| `extract` | Rule-based: events → typed atomic memories with confidence and provenance                          | redact                  |
| `anchor`  | tree-sitter symbol resolution + hashing; staleness checks against the working tree                 | store                   |
| `store`   | The only SQLite access: schema, migrations, FTS5, dedup, supersede, ledger                         | none                    |
| `pack`    | Retrieve + rank + fit to a token budget; log to the ledger                                         | store, anchor           |
| `mirror`  | Write and read one markdown file per memory; rebuild the DB from the mirror                        | store                   |
| `mcp`     | stdio MCP server, 4 tools                                                                          | pack, store             |
| `cli`     | Argument parsing and output formatting only                                                        | all                     |

## Boundaries

- Only `store` runs SQL.
- Only `harness` knows agent-specific formats. Everything downstream sees `Event` and `Memory`.
- Text passes `redact` before it reaches `store`, `mirror`, or `archive` metadata. Archive bytes are the original session, kept with the same permissions as the source.
- `main.rs` and `cli` hold no business logic.

## Data flow

1. `recalla backfill`: harness finds sessions → ingest reads from the last checkpoint → archive copies the file → events are redacted → extract produces memories → store dedups (content hash + key) and inserts → mirror writes `.md` → checkpoint is saved.
2. Capture hook (Stop/PreCompact) runs the same pipeline on just the current session, incrementally.
3. SessionStart hook: detect the project (git root + remote) → anchor refreshes staleness for that project's memories → pack builds a pack within budget → stdout to the agent → ledger row.
4. MCP `recall` / `why` / `remember` / `review` work as thin wrappers over pack and store.

## Cross-cutting

- **Errors:** `thiserror` in the lib, `anyhow` at the CLI edge. No silent catches. A hook failure must never block the agent: log it and exit 0 with an empty pack.
- **Logging:** `tracing` to stderr and `~/.local/state/recalla/log`. Never log memory content at info level.
- **Config:** `~/.config/recalla/config.toml`, validated at startup. Env `RECALLA_HOME` overrides the data dir.
- **Auth:** none (local, single user). File permissions are 0600/0700.

## Store schema (v1)
- `memories`: one row per atomic memory.
  - `uid`: blake3 of kind|scope|normalized text (deterministic, idempotent).
  - Scope columns, confidence, importance, status, `superseded_by`, `seen_count`, valid-time interval.
- `provenance`: 1..n per memory. Unique on (memory, harness, session, message, line).
- `anchors`: code locations (path, symbol, lang, content_hash, commit). Unique on (memory, path, symbol).
- `relations`: (src, dst, kind). Currently only `supersedes` (new → old).
- `memories_fts`: FTS5 external-content table over `text` only, synced by triggers. Kind is filtered via SQL, not FTS.
- Migrations: an append-only list + `PRAGMA user_version`, applied in one IMMEDIATE transaction.

## Redaction (`redact`)
- `redact(&str) -> Redacted { text: Cow, findings }`. Clean text is borrowed. Each secret becomes `[REDACTED:<rule>]`. Idempotent.
- Layer 1, known formats: one regex per rule plus a per-rule validator; boundaries are checked in code (CJK- and JSON-escape-safe).
- Layer 2, keyword assignments: redacts only the value.
  - Any key word ending in a secret word counts (`PGPASSWORD`, `apitoken`).
  - Unquoted values run to whitespace; JSON values stop at structure.
  - Code is exempt only when the whole value is a call, path or reference.
  - Placeholders are exempt only as whole values.
  - Values scan at most 4 KB ahead and the scan always moves forward (linear-time).
- Headers: any `Authorization` scheme and `Cookie`/`Set-Cookie` values are redacted; URL passwords run to the last `@`.
- Layer 3, entropy: tokens of 24+ chars with 3+ char classes and entropy ≥ `min(4.0, log2(len)-0.85)`.
  - Excludes hex/UUID/identifiers/camelCase, base64 prose, and exact-shape agent ids (`toolu_…`, `msg_…`).
  - A token containing `/` is skipped only if it is a clear path; otherwise it is judged whole.
- Layers never defer to each other; a residual pass redacts any recognised secret key/header value left uncovered (D-009). Input is expected JSON-decoded by the harness.
- `redact()` reruns detection on its output (≤4 passes) and maps findings back, so idempotence holds by construction (D-010).
- Merge gate: `tests/realistic_corpus.rs`, 108 real-world credential templates.
- Spans merge (earliest start, then longest, then lowest layer). Offsets are byte offsets on char boundaries in the original text.
- Measured on real sessions: 46 MB/s release. Most layer-3 hits are structural blobs (`signature`, `encrypted_content`, base64 images), which harness parsers should drop before redaction.

## Harness: Claude Code (`harness::claude`)
- Root: `$CLAUDE_CONFIG_DIR` or `~/.claude`.
- Discovery: `projects/*/*.jsonl` plus `projects/*/*/subagents/*.jsonl`. Symlinks are skipped at every level, including symlinked directories such as `subagents/`.
- `parse_session(path, from_offset)` returns `(events, stats, next_offset)`. It reads line by line, never consumes a partial last line, and keeps line numbers absolute on resume.
- Kept: user text, assistant text, tool calls (compact JSON input), tool results, compaction summaries.
- Dropped and counted in `ParseStats`: thinking, images, telemetry/UI record types, `isMeta` records, and `<system-reminder>`/command-wrapper spans.
- Event text is emitted whole. Ingest redacts FIRST, then truncates to 32 KB (`truncate_middle`), so a cut can never split a secret.
- Output is decoded text. It is NOT redacted; ingest must redact every event's text, including tool-call JSON.
- Measured on real logs: 258 MB/s, 0 malformed lines across 26 files.

## Ingest (`ingest`, `recalla backfill`)
- Data dir: `--data-dir`, else `$RECALLA_HOME`, else `$XDG_DATA_HOME/recalla`, else `~/.local/share/recalla`. Dirs are 0700; the DB and its WAL/SHM are 0600.
- Per file:
  1. Identity check decides new / resume / restart / unchanged. It uses blake3 of the ingested prefix (≤4 KB) plus a hash of the last ingested line, and the checkpoint must sit on a `\n`. The root is canonicalized so one file has exactly one source.
  2. Read `[offset, len)` once; parse (`parse_bytes`) and archive from that same buffer.
  3. Per event: `redact` text, tool name, cwd and branch, then `truncate_middle`. ToolCall JSON is redacted per string leaf and then as a whole, and truncated leaf-first so it stays valid (D-015).
  4. zstd segment of the raw bytes `[offset, next_offset)`.
  5. One IMMEDIATE txn: optional reset + events + checkpoint + segment row.
- Re-runs are idempotent: `UNIQUE(source_id, line, seq)`. One line can emit several events.
- Archive: `archive/<blake3>.zst`, content-addressed, written once (tmp + fsync + rename). Holds RAW bytes (local copy, 0600). `--no-archive` opts out.
- Measured on real logs: 30.4 MB in 0.65 s (28 MB peak RSS). DB ~20% of raw; archive ~16% of raw. Re-run is a no-op in 1 ms.

## Store schema (v2)
- `sources`: one checkpoint per log file (identity, size, next_offset).
- `events`: redacted, truncated events with full provenance, plus `events_fts` (FTS5).
- `segments`: archive pieces per source.
- Provenance uniqueness now includes `source_path`.

## Extractor (`extract`, `recalla extract`)
- Reads redacted `events` incrementally from the `extract_state` checkpoint (written after each 1000-event batch). Never reads raw logs or the archive.
- Each event gets a context window of up to 8 earlier events in the same session; project keys are cached per cwd.
- Rules are pure functions over `EventCtx`. Each one returns typed `Candidate`s, which pass through `Store::insert` (uid dedup bumps `seen_count`):
  - **preference:** human user text with directive markers.
  - **decision:** decision phrases, or `## D-NNN` blocks written to DESIGN_DECISIONS.md.
  - **gotcha:** a cause explanation after a tool error in the current exchange.
  - **procedure:** a successful build/test/lint command, normalized to its core (`cd X && make check | tail` becomes `make check`).
  - **fact:** narrow `X is on <version/path>` shapes.
  - **task:** TodoWrite / TaskCreate items; completed items get archived.
- Subagent (sidechain) "user" text is the orchestrator's instructions, not the human's, so it never yields preferences, facts or user decisions.
- Questions are skipped, including ones without a `?`.
- `EXTRACTOR_VERSION` bump or `--rebuild`: archive backfill-only memories, then re-extract.
- Eval gate: `tests/extract_eval.rs` over 11 synthetic sessions / 53 labels requires zero false positives. Real-data review is a standing step, because an eval written alongside the rules overfits.
- Measured: 2,751 real events extracted in 26 ms.

## Context packs (`pack`, `recalla pack|stats|why`)
- Candidates: active + needs_reverify memories in Global ∪ Project(key) ∪ Module(key), where key = `project_key(cwd)` (git toplevel).
- Score = kind weight × confidence × (1 + ln seen_count) × recency (90-day half-life floor 0.5). `--query` multiplies BM25 hits by 1.5 + normalized rank. Ties break on uid.
- Greedy fill in rank order with per-section caps (procedures ≤6 …); fixed section order: Preferences, Gotchas, Decisions, Procedures, Tasks, Facts, Needs re-verify ⚠.
- Budget enforced on `est_tokens = ceil(bytes/3) + 2/line`, which overestimates real tokens (D-016). A property test covers 500 random sets.
- Ledger: `injections` table (schema v4), one row per non-empty pack with via cli|hook|mcp. `stats` reports totals, per project and most-injected memories.
- `why <uid-prefix>`: memory, provenance and the stored (redacted) source event text.
- Preferences are project-scoped unless explicitly global ("in all projects", "everywhere"), so one repo's rules never leak into another's pack.
