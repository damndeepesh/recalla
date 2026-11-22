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
- `make secrets`: gitleaks over full history (also a CI job)

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
