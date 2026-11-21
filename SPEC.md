# Spec

## Problem

- Coding agents forget everything between sessions. Users re-explain decisions, and agents repeat past debugging.
- Existing memory tools:
  - Only capture going forward (no real backfill of existing history).
  - Serve stale facts after code changes, unmarked.
  - Burn tokens: 27–32 MCP tools ≈ 5k tokens per turn, LLM observers adding ~64% spend.
  - Run heavy daemons (CPU and process leaks).
  - Leak secrets.
- Evidence: validate research, 2026-11-21. Competitors: ai-memory, claude-mem, ICM, engram, memdex, agentmem, workshop.

## Users

- Heavy agent user (Claude Code + Codex): wants months of past sessions to become useful memory without re-explaining.
- Multi-agent dev: wants one memory every agent reads, consistent and cheap in tokens.
- Privacy-conscious dev: wants it offline with no LLM key, and secrets never stored.

## Scope

### Must (MVP)

- Backfill Claude Code + Codex JSONL into atomic memories.
  - acceptance: re-running imports 0 new rows; a killed run resumes from its checkpoint; every memory resolves to (harness, session_id, message_uuid, file, line).
- Memory kinds: decision, fact, procedure, gotcha, preference, task.
  - acceptance: the extractor yields typed memories from fixtures with precision ≥0.8 on the labeled set.
- Code anchors (path + symbol + content hash + commit) via tree-sitter.
  - acceptance: editing an anchored symbol marks the memory `needs_reverify`; editing another symbol in the same file does not; `confirm` re-anchors.
- `pack --budget N` builds a context pack ≤N tokens.
  - Priority order: constraints/warnings > decisions > current state > rest. Stale memories are labeled.
  - acceptance: the token count never exceeds the budget; every pack is logged in a ledger.
- Ledger: `stats` shows tokens injected per session and per memory.
- MCP server (stdio) with 4 tools: `recall`, `remember`, `review` (confirm/forget), `why`.
  - acceptance: `tools/list` ≤1k tokens, measured in a test.
- Hooks for Claude Code (SessionStart inject, Stop/PreCompact capture) and Codex.
  - acceptance: install and uninstall are idempotent and never clobber existing hooks or files.
- Layered secret redaction (known-format rules + entropy) before anything is persisted.
  - acceptance: the secret fixture corpus is 100% redacted in DB and mirror.
- Markdown mirror: one `.md` per memory, with frontmatter for provenance and anchors.
  - acceptance: an agent can read the mirror with no engine; a rebuild from the mirror gives an identical DB.
- Immutable raw archive: zstd-compressed copies of ingested sessions.
  - acceptance: the original is byte-identical after decompression.

### Later

- Contradiction detection and resolution (same anchor/key, valid-time intervals).
- Optional local-LLM extraction (Ollama/llama.cpp), feature-flagged.
- Optional local embeddings, hybrid with BM25 via RRF.
- Serverless sync of the mirror (git/Syncthing) with deterministic merge.
- Benchmarks: agent-memory-bench arm, AML Coding entry, own coding-fault eval.
- More harnesses: Gemini CLI, OpenCode, Cursor.

### Non-goals

- Hosted service or cloud dependency.
- Required LLM or API key.
- Graph database.
- Large MCP surface (>4 tools).
- Out-featuring ai-memory on harness count or team auth.

## Invariants (must always hold)

- No network and no LLM calls unless an opt-in feature is enabled.
- No raw secret reaches disk outside the user's original session files.
- Source sessions are never modified. Memories are never hard-deleted (supersede or archive, always reversible).
- Every memory has provenance back to a source message or an explicit `remember` call.
- Packs never exceed their token budget.
- Hook commands finish in <100 ms p95 on a 100k-memory store; a backfill of 300 MB finishes in <5 min on an M-series laptop.

## Constraints

- Single static-ish binary for macOS and Linux (Windows later).
- Apache-2.0.
- Offline by default.
- Must not break or overwrite existing agent configs.
