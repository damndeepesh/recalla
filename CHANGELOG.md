# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) · Versioning: [SemVer](https://semver.org/).

## [Unreleased]
### Added
- Initial project scaffold
- Core memory model (`Kind`, `Scope`, `Status`, `Provenance`, `Anchor`) with deterministic blake3 uids
- SQLite store: migrations, dedup with provenance merge, supersede/archive/restore, FTS5 BM25 search with safe query escaping
- Secret redaction: known credential formats, keyword-assignment values, entropy catch-all; linear-time, idempotent
- Claude Code harness: session discovery (incl. subagents) and streaming, resumable JSONL parser into normalized events
