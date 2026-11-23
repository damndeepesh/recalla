# Roadmap

## MVP: backfilled, anchored, token-budgeted memory for Claude Code + Codex (status: done, v0.1.0)

- Backfill with message-level provenance
- Symbol anchors + soft staleness
- `pack --budget` + ledger
- 4-tool MCP server + hooks
- Secret redaction, markdown mirror, raw archive

## v1.0: trusted and proven

- [x] `recalla ui`: local dashboard (sessions, memories, what Claude got, connect), built and in review; ships in v0.2.0
- Better extraction: bare imperatives, past-tense decisions, gotcha-to-error matching (issues #13 to #17)
- Contradiction resolution
- Optional local embeddings (hybrid RRF)
- Native Codex hooks (today Codex integrates via `backfill` and MCP)
- A real-history false-positive study (issue #8) and a coding-memory benchmark
- Gemini CLI, OpenCode, Cursor harnesses
- Published benchmarks: agent-memory-bench arm + own coding-fault eval, losses included
- `brew` / `cargo install` / prebuilt binaries

## Later

- Optional local-LLM extraction
- Serverless mirror sync (git/Syncthing)
- Desktop viewer (Tauri)
- Hosted team sync (open-core tier)
