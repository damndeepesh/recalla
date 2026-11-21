# Design Decisions

Append-only. To change one, add a new entry that says "Supersedes D-NNN".

## D-001 Rust, single daemonless binary (2026-11-21)

- Context: Hooks run on every agent session start and prompt, so startup and memory dominate. The main competitors' biggest complaints are daemons, CPU spikes and process leaks (claude-mem: Bun + Chroma; ai-memory: always-on server).
- Decision: Rust, one binary, short-lived processes. The only long-lived mode is the MCP stdio server.
- Why: millisecond startup, low RAM, fast bulk JSONL/zstd parsing, a mature tree-sitter + rusqlite ecosystem, single-binary distribution.
- Rejected: Go (weaker tree-sitter bindings, SQLite via cgo); TS/Bun (heavier runtime, the claude-mem resource trap).

## D-002 SQLite + FTS5 as store, markdown mirror as portable format (2026-11-21)

- Context: Retrieval needs to be fast and indexed, but the user wants a format agents can read without the engine.
- Decision: SQLite (WAL) + FTS5 BM25 is the source of truth, with a one-`.md`-per-memory mirror that can rebuild the DB. Vectors are optional, later.
- Why: zero-service, embedded, proven; BM25 is strong for code terms; the mirror gives portability and git-friendliness.
- Rejected: vector DB / graph DB (heavy, opaque); markdown-only (slow queries, no ledger or anchors).

## D-003 Atomic memories with message-level provenance (2026-11-21)

- Context: ai-memory uses pages with coarse provenance. ICM's import loses its source. Users need "why does the agent believe this?".
- Decision: The unit is an atomic memory (kind, text, confidence, importance, scope), with provenance `(harness, session_id, message_uuid, file, line, commit)` or an explicit `remember` origin.
- Why: auditability, precise supersede/dedup, a wedge ai-memory can't copy without re-architecting.
- Rejected: page/session summaries (coarse); LLM-written observations (cost, nondeterminism).

## D-004 Rule-based extraction by default, no LLM (2026-11-21)

- Context: LLM observers cost about 64% extra spend (claude-mem #3848), and mem0 audits found mostly junk.
- Decision: Deterministic rule and heuristic extraction, measured against a labeled eval set. A local-LLM pass is a later opt-in feature.
- Why: offline, free, reproducible, testable.
- Rejected: required cloud LLM; default to the user's agent CLI (ICM's CPU-loop bug #322).

## D-005 Apache-2.0, no monetization in MVP (2026-11-21)

- Context: The goal is adoption. Competitors use MIT or Apache. A hosted team-sync tier may come later.
- Decision: Apache-2.0.
- Why: permissive for adoption, adds a patent grant, compatible with open core later.
- Rejected: MIT (no patent grant); AGPL (deters adoption and integrations).

## D-006 Core store dependencies: rusqlite, thiserror, blake3 (2026-11-22)

- Context: The core model + store needs an embedded DB with FTS5, typed errors, and deterministic ids; hooks, CLI and MCP run as concurrent processes.
- Decision: `rusqlite` (feature `bundled`, so FTS5 is always present), `thiserror` 2 (typed `StoreError`), `blake3` (deterministic content `uid`: idempotent re-imports); dev-only `tempfile` (file-backed DB tests). Migrations are a hand-rolled `PRAGMA user_version` list.
- Why: SQLite WAL handles multi-process access; BM25 comes from FTS5 with no extra index; blake3 is fast, tiny, and needs no id coordination across machines.
- Rejected: redb (single-process lock); Tantivy (single writer, heavier; maybe later as an optional search booster); rusqlite_migration (hand-rolled user_version is enough); uuid/ulid crates (random ids break idempotent re-imports).
