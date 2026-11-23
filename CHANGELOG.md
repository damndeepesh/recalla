# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) · Versioning: [SemVer](https://semver.org/).

## [Unreleased]

### Added
- `recalla forget <id>`: archive a wrong memory from the CLI (undo with `recalla confirm`). Closes #27.

### Added
- `bench/` (`membench`): a neutral benchmark for coding-agent memory tools. Ground truth is mined from the git history of six public repositories (18 suites, 240 probes), scored on right-fact recall at a token budget, noise, staleness against code changes, provenance, extraction and token savings, with a generic MCP adapter for any memory server, a written report (totals, tune vs holdout, where each tool loses) and raw per-probe JSON. `make bench`, `make bench-check`. `membench real` runs the same benchmark on your own repository's history, locally. See `bench/README.md` and `bench/SCORING.md`.

### Changed
- Extractor precision: a decision needs at least 5 words and an object ("Decided upfront, not bolted on later." is dropped) (closes #15); test-run procedures collapse to the base command, so `cargo test --test x name` and `go test ./pkg/...` become `cargo test` / `go test` (closes #16); a gotcha's explanation must share a word with the failing command or output (closes #17).
- `recalla pack --query` (and the MCP `recall` query) now lifts memories sharing any word with the query, BM25-ranked, instead of requiring every word, so a task description actually finds related memories. `recalla search` still requires all words.
- `recalla pack --query` now focuses the pack on the task: memories anchored to a file the query names rank first, weak matches (under a quarter of the best score) and near-duplicates are dropped, and preferences are always kept. A query that matches nothing leaves the pack unchanged. On the benchmark's holdout probes this cut the pack from 869 to 518 tokens while recall went from 0.91 to 0.95.

## [0.3.0] - 2026-11-23

### Added
- Extractor: bare imperatives with a scope or limit word ("Use terraform plan before any apply", "Keep the binary under 5 MB") are now preferences, and "Switched X to Y" (up to 6 words between) is a decision. Closes #13, #14. Eval on the synthetic corpus: recall 0.92 to 1.00, precision unchanged at 1.00.
- `recalla backfill --dir PATH`, `--session ID|FILE` (both repeatable), `--list` and `--pick`: import one project or a few sessions instead of the whole history. `--pick` asks for Claude Code or Codex, lists that tool's sessions, and imports the numbers you choose. A selector that matches nothing is an error and writes nothing.

### Changed
- `recalla ui`: redesigned dashboard (new visual system with self-hosted Bricolage Grotesque and Geist, bento Home with activity and token charts, memory browser, sessions, context packs, setup). Memory text renders `code` spans.

### Fixed
- Landing page: the "Watch a memory go stale" steps no longer split inline code and emphasis into separate columns.
- `recalla ui`: a stray "null" on Home, charts whose legends did not match what was drawn, and Home rows that left empty columns.

## [0.2.1] - 2026-11-23

### Changed
- Landing page redesigned: new layout, self-hosted fonts (Instrument Sans, JetBrains Mono; no third-party requests), refreshed dark theme and social preview image. The page guard now also checks fonts and multi-line third-party tags.

### Fixed
- `recalla ui`: the browser showed "Unauthorized" right after login. The session cookie was `SameSite=Strict`, which browsers withhold when the navigation started from the local `file://` redirect page. It is now `SameSite=Lax` (still `HttpOnly`, per-port, 12 h idle); mutating requests keep their `Host`, `Origin` and CSRF checks. Guarded by a real-browser check, `scripts/ui-browser-check.sh` (D-024)

## [0.2.0] - 2026-11-23

### Security
- Dashboard threat model: binds 127.0.0.1 only; login is a one-time code valid for 60 s, delivered through a private 0600 redirect page (never argv or a URL in the process list); per-port session cookie (`HttpOnly`, `SameSite=Strict`) that expires after 12 h idle; `Host`, `Origin` and CSRF-header checks on every request; strict CSP and no third-party requests; reads use a read-only database connection. See ARCHITECTURE.md and D-022/D-023

### Fixed
- Landing page works when a proxy serves it at `/recalla` without the trailing slash (`<base href="/recalla/">`, in-place anchor scrolling), guarded by `scripts/web-check.sh`

### Added
- `recalla ui`: local dashboard served from the single binary (no install, no build step, no telemetry)
  - Views: sessions, memories, what Claude got (each injected pack, its tokens and the saving versus dumping every memory), connect (hooks and MCP status)
  - One-time login link printed on start and opened in your browser
  - From the browser: install/uninstall hooks, confirm/forget/restore a memory, test recall for a project, import history
  - Schema v8 records session id, pack text and candidate tokens per injection; existing databases migrate automatically
- Landing page in `web/` (static, no third-party requests), deployed to GitHub Pages by `.github/workflows/pages.yml` and served at deepeshgupta.dev/recalla; `scripts/web-check.sh` guards its privacy promise in CI
- Release downloads for macOS (arm64, x86_64) and Linux (x86_64, arm64); Windows users run the Linux build in WSL until native support lands (#32)

## [0.1.0] - 2026-11-22
### Changed
- Published crate excludes internal agent state, CI and dev-only files

### Fixed
- MCP: Global memories are read-only (no confirm/forget), and `why` hides their sources

### Added
- Markdown mirror: one `.md` file per memory under `<data>/mirror` (own YAML subset, `format: 1`), kept in sync incrementally after backfill/extract, hooks, MCP `remember`/`review` and `confirm`/`check`; `recalla mirror [--full]`; schema v7 (`mirror_dirty`, trigger-maintained, clock-independent); anchor changes now bump `updated_at`
- `recalla rebuild --from-mirror [DIR] [--yes] [--allow-drop]`: verified, all-or-nothing restore of the memory layer in one transaction on the live DB (safe while `recalla mcp` or hooks run), with a `VACUUM INTO` backup; refuses unredacted or hostile mirror files
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
- Performance: `make perf` (319.5 MB synthetic backfill in 21.6 s; session-start p95 12.6 ms on 100k memories); schema v6 index on (scope_project, status)
- Codex harness: `backfill` imports Codex CLI sessions too (`--harness`, `--codex-root`); one memory carries provenance from both agents
- `recalla mirror` and `recalla rebuild --from-mirror`: one readable markdown file per memory; rebuild verifies ids, refuses tampered, unredacted or hostile files, backs up with `VACUUM INTO` and replaces the memory layer in one transaction, so it is safe while Claude Code or `recalla mcp` are running
- `make e2e`: 61-check end-to-end functional script (planted secrets, hooks safety, MCP both eras, anchors staleness, mirror round trip)
- MCP `remember` anchors memories to the code they name, so agent-saved memories go stale like extracted ones
- Code anchors: memories pinned to tree-sitter symbols (Rust, Python, Go, TS/TSX, JS); `needs_reverify` with a reason when the symbol changes or disappears; `recalla check`, `recalla confirm`; packs show the reason
- `redact_path` for cwd/branch (no entropy layer) so project keys survive random path components
- Claude Code hooks: `recalla hook session-start|capture`, `recalla hooks install|uninstall|status` (never block; settings backed up, atomic, order-preserving)

[Unreleased]: https://github.com/damndeepesh/recalla/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/damndeepesh/recalla/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/damndeepesh/recalla/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/damndeepesh/recalla/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/damndeepesh/recalla/releases/tag/v0.1.0
