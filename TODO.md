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
- [ ] Markdown mirror + `recalla rebuild`
- [ ] Codex harness
- [ ] Perf pass: 300 MB backfill <5 min, hook p95 <100 ms
