# Plan: Claude Code harness

## Goal

Find Claude Code session logs and stream them into normalized, JSON-decoded `Event`s. Each event keeps exact provenance (session, message uuid, line, byte offset) and drops noise. Redaction and storage are out of scope here: the ingest pipeline (next TODO item) calls `redact` on event text, then `Store::insert`.

## Observed format (from local logs, key names only)

- Layout: `$CLAUDE_CONFIG_DIR` or `~/.claude`, then `projects/<encoded-cwd>/<session-uuid>.jsonl`.
  - Subagents: `<session-uuid>/subagents/agent-<id>.jsonl` (+ `.meta.json`).
  - Large tool outputs persisted to `<session-uuid>/tool-results/*.txt`.
- One JSON record per line. Lines can be up to ~180 KB.
- 20 record `type`s. Conversation lives only in `user` and `assistant`; the rest are telemetry or UI state (`api-request*`, `data-flow-records`, `attachment`, `mode`, `file-history-*`, …).
- Common fields: `uuid`, `parentUuid`, `sessionId`, `timestamp`, `cwd`, `gitBranch`, `isSidechain`, `isMeta`, `version`.
- `message.content` is a string or a list of blocks:
  - `text`
  - `thinking` (with signature)
  - `tool_use` (`id`, `name`, `input`)
  - `tool_result` (`tool_use_id`, `content` str|blocks, `is_error`)
  - `image`
  - `tool_addition`

## Design

- `harness` module: a `Harness` trait with `name()`, `discover(root) -> Vec<SessionFile>`, and `parse(reader, from_offset) -> impl Iterator<Item = Result<Event>>`, plus `ParseStats`.
- `Event` fields:
  - `harness`, `session_id`, `message_uuid`, `parent_uuid`
  - `line` (1-based), `byte_offset` (start of line, for resume)
  - `timestamp` (unix ms), `cwd`, `git_branch`, `sidechain: bool`
  - `kind`:
    - `UserText`
    - `AssistantText`
    - `ToolCall { id, name, input }` (compact JSON)
    - `ToolResult { tool_use_id, is_error }`
  - `text`
- Keep: user text (non-meta), assistant text, tool calls (name + compact input), and tool results (text parts).
- Drop:
  - `thinking` and its signature (private reasoning, mostly noise)
  - image and base64 blocks
  - `tool_addition`
  - every non-conversation record type
  - `isMeta` user records
  - `<system-reminder>…</system-reminder>` spans inside user text (harness-injected, not the user's words)
- Size cap: event text over 32 KB is truncated to head + tail with a `[…N bytes omitted…]` marker. A huge tool dump never becomes one giant memory.
- Robustness:
  - Malformed JSON, unknown types and missing fields are counted in `ParseStats` and skipped, never fatal.
  - Parsing is lenient (`serde_json::Value` + typed accessors), so schema drift across Claude Code versions doesn't break ingest.
- Streaming: `BufRead` line by line with byte offsets. Seek to `from_offset` to resume. A partial last line (file still being written) is not consumed.
- Privacy: the harness never logs content, only counts.

## Steps

- [x] 1. Add `serde_json` dep + D-012 (why: the JSONL is JSON; lenient `Value` parsing beats brittle typed structs for an external, versioned format)
- [x] 2. `harness/mod.rs`: `Event`, `EventKind`, `SessionFile`, `ParseStats`, `Harness` trait, text truncation helper
- [x] 3. `harness/claude.rs` discovery: config dir resolution, project dirs, main + subagent session files, sorted deterministically. Ignore symlinks that escape the root.
- [x] 4. `harness/claude.rs` parser: record to events, block mapping, system-reminder stripping, meta skipping, offsets, partial-line handling, resume from offset
- [x] 5. Synthetic fixtures `tests/fixtures/claude/` (hand-written, schema-faithful, no secrets) + `tests/claude_harness.rs`:
  - golden events
  - malformed and unknown lines
  - resume from offset gives the same tail
  - partial last line
  - multibyte
  - huge line truncated
  - subagent discovery
- [x] 6. Local smoke run over real sessions (counts only, not committed): events by kind, parse errors, throughput
- [x] 7. Docs: ARCHITECTURE harness section, TODO tick, CHANGELOG, state.yaml

## Files touched

- `Cargo.toml`/`Cargo.lock`, `src/lib.rs`, `src/harness/{mod.rs,claude.rs}`
- `tests/claude_harness.rs`, `tests/fixtures/claude/**`
- DESIGN_DECISIONS.md, ARCHITECTURE.md, TODO.md, CHANGELOG.md, `.agent/state.yaml`

## Reused modules

- None directly. Output feeds `redact::redact` and `store::Store::insert` in the ingest item.
- Provenance fields map 1:1 onto `model::Provenance` (harness, session_id, message_uuid, source_path, line).

## Risks

- **Schema drift** in future Claude Code versions. Mitigation: lenient parsing, stats on unknowns, fixtures per observed shape.
- **Dropping useful signal:** thinking blocks sometimes hold the "why". Accept for MVP; an opt-in flag can come later.
- **Huge files / huge lines:** streaming plus a per-event cap. No whole-file reads.
- **Path safety:** discovery stays under the config root, does not follow symlinks out, and never writes.
- **Fixture realism:** hand-written fixtures may miss real quirks. The step-6 smoke run over real logs catches parse errors without committing data.

## Test plan

- Unit: block mapping per type, system-reminder stripping, truncation boundaries (char-safe), timestamp parsing.
- Integration over fixtures: exact expected event sequence, stats counts, resume equivalence, partial-line safety, subagent files discovered.
- Smoke (local only): 0 hard errors over all real sessions; report counts only.
- `make check` + `make secrets` green.
