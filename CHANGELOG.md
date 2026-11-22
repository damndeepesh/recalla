# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) · Versioning: [SemVer](https://semver.org/).

## [Unreleased]
### Fixed
- MCP: Global memories are read-only (no confirm/forget), and `why` hides their sources

### Added
- Initial project scaffold
- Core memory model (`Kind`, `Scope`, `Status`, `Provenance`, `Anchor`) with deterministic blake3 uids
- SQLite store: migrations, dedup with provenance merge, supersede/archive/restore, FTS5 BM25 search with safe query escaping
- Secret redaction: known credential formats, keyword-assignment values, entropy catch-all; linear-time, idempotent
- Claude Code harness: session discovery (incl. subagents) and streaming, resumable JSONL parser into normalized events
- `recalla backfill`: resumable, idempotent import of Claude Code sessions into redacted events, with a content-addressed zstd archive of raw bytes (`--no-archive`, `--dry-run`, `--json`)
- Store schema v2: sources, events (+FTS5), segments; provenance uniqueness includes source path
- Rule-based extractor: preferences, decisions (incl. ADR files), gotchas, procedures, facts and tasks from stored events; incremental and checkpointed; `recalla extract`, `recalla search`; backfill runs extraction (`--no-extract` to skip)
- Store schema v3: extraction checkpoint
- Context packs: `recalla pack` (token-budgeted, ranked, per-project), injection ledger (`recalla stats`, schema v4), provenance view (`recalla why`)
- Preferences are project-scoped unless explicitly global (extractor v2)
- `recalla mcp`: stdio MCP server (2026-07-28 + legacy) with `recall`, `remember`, `review`, `why` (~430 tokens)
- Codex harness: `backfill` imports Codex CLI sessions too (`--harness`, `--codex-root`); one memory carries provenance from both agents
- Code anchors: memories pinned to tree-sitter symbols (Rust, Python, Go, TS/TSX, JS); `needs_reverify` with a reason when the symbol changes or disappears; `recalla check`, `recalla confirm`; packs show the reason
- `redact_path` for cwd/branch (no entropy layer) so project keys survive random path components
- Claude Code hooks: `recalla hook session-start|capture`, `recalla hooks install|uninstall|status` (never block; settings backed up, atomic, order-preserving)
