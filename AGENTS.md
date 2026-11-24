# Recalla

Local-first memory engine for coding agents. It backfills session history into atomic, provenance-linked, code-anchored memories and serves token-budgeted context packs.

## Stack

- Rust (stable, edition 2024) · SQLite + FTS5 (rusqlite, bundled) · `cargo test` · rustfmt + clippy (pedantic, `-D warnings`)

## Commands

- `make setup`: check toolchain, fetch deps
- `make dev`: run the CLI locally
- `make fmt`: format
- `make check`: fmt-check + clippy + tests (must pass before push)
- `make build`: release binary (`target/release/recalla`)
- `make perf`: SPEC perf targets on synthetic data (release, ignored tests in `tests/perf.rs`, ~1 min; results in ARCHITECTURE.md)
- `make secrets`: gitleaks over full history (also a CI job)
- `make e2e`: end-to-end functional checklist on the release binary (88 checks)
- `make live`: live round trip with the real `claude` (and `codex`) CLIs, scratch data dir; needs installed hooks, not in CI
- `recalla backfill [--dir P]... [--session ID|FILE]... [--list] [--pick] [--harness claude|codex|all] [--root R] [--codex-root C] [--data-dir D] [--no-archive] [--no-extract] [--dry-run] [--json]`: import + extract
- `recalla extract [--rebuild]` / `recalla search <q> [--kind K] [--project P]`: re-extract / inspect memories
- `recalla pack [--budget N] [--query Q] [--format md|json] [--explain]` / `recalla stats` / `recalla why <id>`: context pack, ledger, provenance
- `recalla doctor [--data-dir D] [--settings P] [--json]`: wiring/health check (hooks, MCP for Claude and Codex, db, last capture/injection); exit 1 on a failed check
- `recalla check [--project P] [--json]` / `recalla confirm <id>` / `recalla forget <id>`: flag memories whose anchored code changed / re-anchor one / archive a wrong one
- `recalla promote <id> [--global | --project KEY]`: move a preference/decision/etc. to global or one project (supersedes the original; CLI + UI only, never MCP)
- `recalla mirror [--full] [--json]` / `recalla rebuild --from-mirror [DIR] [--yes] [--allow-drop]`: sync the markdown mirror (also automatic; `--no-mirror` on backfill/extract) / restore the memory layer from it (dry run without `--yes`; safe while mcp/hooks run; `VACUUM INTO` backup `recalla.db.bak-<ts>`; `--allow-drop` to remove memories absent from the mirror)
- `recalla mcp [--data-dir D] [--project P]`: MCP stdio server (tools: recall, remember, review, why)
- `recalla hooks <install|uninstall|status> [--settings P] [--dry-run]` / `recalla hook <session-start|capture>`: Claude Code hooks (never block, log to `<data>/log/hook.log`)
- `recalla ui [--port N] [--no-open] [--data-dir D]`: local dashboard server on 127.0.0.1 (prints a one-time, 60 s login link; Ctrl-C to stop)
- `make bench` / `make bench-check`: benchmark crate in `bench/` (`REPOS=<clones>` for real worktrees, `RECALLA=1` to include Recalla); see `bench/README.md`, `bench/SCORING.md`
- `cargo test --test extract_eval -- --nocapture`: extractor precision/recall table

## Layout

- `src/lib.rs`: library root. One module per component (see ARCHITECTURE.md)
- `src/main.rs`: CLI entry only; no logic
- `tests/`: integration tests + `tests/fixtures/` (synthetic, redacted transcripts only)

## Rules

- Read `.agent/state.yaml` + TODO.md before working. Keep them current per `.agent/README.md`.
- Scope and requirements → SPEC.md. Past decisions → DESIGN_DECISIONS.md (don't re-litigate them).
- Daemonless: every command is a short-lived process. No background servers except `recalla mcp` (stdio).
- No network, no LLM calls in the default path. Optional features must be feature-flagged.
- Redact before persist: no raw text reaches SQLite or the mirror without passing `redact`.
- Raw sessions are never modified; memories are never hard-deleted (supersede/archive only).
- All SQL lives in `store`; other modules don't touch the DB.
- MCP surface ≤4 tools, `tools/list` ≤1k tokens. Adding a tool needs a DESIGN_DECISIONS entry.
- Never commit real session transcripts; fixtures must be synthetic.
- Never write secret-shaped literals (incl. `password=<word>`); build them at runtime or use a `{S}` template placeholder. Run `make secrets` before pushing.
- Each new dependency needs a DESIGN_DECISIONS entry.
