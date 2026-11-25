# Plan: more harnesses (v0.5)

Order: opencode, then Copilot CLI, Gemini CLI, Cursor (JSONL transcripts only). Each on its own branch, `Harness` impl + synthetic fixtures + tests + doctor/hooks/MCP wiring where the agent supports it.

## Verified on disk (2026-11-24, structure only)
- opencode: `~/.local/share/opencode/opencode.db` (SQLite, WAL). Tables `session(id, directory, title, time_*)`, `message(id, session_id, time_created, data json{role})`, `part(id, message_id, session_id, time_created, data json{type: text|reasoning|tool|step-start|step-finish})`. Tool part: `{tool, callID, state{status,input,output,title,time}}`. Fixture data must be synthetic.
- Copilot CLI / Gemini CLI / Cursor: no sessions on this machine; formats are from docs/third parties and unverified. Build from fixtures written to the documented shape, mark "unverified on real data" in README until a user confirms.

## Design (opencode, the SQLite case)
Ingest resumes by byte offset in a file; SQLite has none. Decision: materialize, then reuse ingest unchanged.
- `harness::opencode::export(db, out_dir)`: open DB read-only (`mode=ro`, `immutable` not used because of WAL), write one deterministic JSONL per session to `<data>/import/opencode/<session>.jsonl`, one line per message/part ordered by (time_created, id). Append-only by construction, so checkpoints, tail-hash identity, archive and redaction all work as for Claude/Codex.
- `OpencodeHarness` parses that JSONL into `Event`s (user/assistant text, Bash/edit tool calls + results; drops reasoning and step-* parts).
- The materialized file holds raw text, so dir 0700 / file 0600, never in the mirror, deleted by `recalla forget`-style cleanup is out of scope; documented in DESIGN_DECISIONS (new entry D-032).
- Discovery: `--harness opencode`, `--opencode-root` (default `~/.local/share/opencode`, `%LOCALAPPDATA%`-style on Windows unverified).
- No new dependency (rusqlite already present).

## Steps
1. export() + fixtures builder (synthetic DB created in tests) + unit tests
2. OpencodeHarness parse + tests (tool call/result pairing, partial/failed tools)
3. wire discover/ingest/backfill flags, `--list/--pick` grouping, doctor line
4. MCP registration hint for opencode (config `~/.config/opencode/opencode.json`) in `hooks status`/doctor; capture hook only if opencode plugin API verified, else backfill-only
5. docs: README harness table, CHANGELOG, TODO, state; make check + CI Windows

## Out of scope
Copilot/Gemini/Cursor (own branches after this one lands), any LLM, reading Cursor `state.vscdb`.
