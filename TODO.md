# TODO

## MVP

- [x] Core model + store: `Memory`/`Provenance`/`Anchor` types, SQLite schema + migrations, FTS5, dedup, supersede/archive
- [x] Redaction: rules + entropy, secret fixture corpus
- [x] Claude Code harness: locate + parse JSONL into `Event`s (incl. tool calls)
- [x] Ingest pipeline: resumable checkpoints, zstd archive, `recalla backfill` + report
- [x] Rule-based extractor: typed memories with confidence + provenance, labeled eval set
- [x] Pack + ledger: project detection, ranking, token budget, `recalla pack`, `recalla stats`
- [x] MCP server: `recall`, `remember`, `review`, `why` (≤1k tokens)
- [x] Hooks: idempotent install/uninstall for Claude Code (Codex via MCP registration; native Codex capture lands with the Codex harness)
- [x] Anchors: tree-sitter symbols (Rust, TS/JS, Python, Go) + `needs_reverify` + `confirm`
- [x] Markdown mirror + `recalla rebuild`
- [x] Codex harness
- [x] Perf pass: 300 MB backfill <5 min, hook p95 <100 ms

## v0.2.0

- [x] `recalla ui`: local dashboard with connector status (branch feature/ui)
- [ ] PR for feature/ui, then release v0.2.0 (version bump, CHANGELOG, publish, tag) on the user's go

## v0.4.0 (plan: .agent/plans/v0.4.0.md)

- [x] 1 fix/capture-and-e2e: tail catch-up at SessionStart, e2e template fix, `make live`
- [x] 2 feature/doctor
- [x] 3 feature/extract-imperative
- [x] 4 feature/pack-explain
- [x] 5 feature/supersede-contradiction
- [x] 6 feature/promote-global
- [x] 7 feature/triage-ui
- [x] 8 feature/pretooluse-recall (opt-in; per-file bench pending)
- [x] 9 feature/ttl-decay
- [x] fix/windows-full: USERPROFILE, `cmd /C start`, `.exe` hook matching, `cargo test` on Windows CI; grouped `backfill --pick`/`--list`
