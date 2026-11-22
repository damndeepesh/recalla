# TODO

## MVP

- [x] Core model + store: `Memory`/`Provenance`/`Anchor` types, SQLite schema + migrations, FTS5, dedup, supersede/archive
- [x] Redaction: rules + entropy, secret fixture corpus
- [x] Claude Code harness: locate + parse JSONL into `Event`s (incl. tool calls)
- [ ] Ingest pipeline: resumable checkpoints, zstd archive, `recalla backfill` + report
- [ ] Rule-based extractor: typed memories with confidence + provenance, labeled eval set
- [ ] Codex harness
- [ ] Anchors: tree-sitter symbols (Rust, TS/JS, Python, Go) + `needs_reverify` + `confirm`
- [ ] Pack + ledger: project detection, ranking, token budget, `recalla pack`, `recalla stats`
- [ ] Markdown mirror + `recalla rebuild`
- [ ] MCP server: `recall`, `remember`, `review`, `why` (≤1k tokens)
- [ ] Hooks: idempotent install/uninstall for Claude Code + Codex
- [ ] Perf pass: 300 MB backfill <5 min, hook p95 <100 ms
