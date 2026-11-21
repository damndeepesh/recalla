# Recalla

**Memory your coding agents can trust.** Recalla turns your existing Claude Code and Codex history into compact, source-linked memories. It flags a memory when the code it describes changes, and it feeds agents only what fits a token budget you set.

<!-- badges: CI · license · version -->
<!-- demo GIF -->

> Status: early development. Not usable yet.

## Why

- **Your history is wasted.** Months of sessions hold decisions and fixes, and most memory tools only capture from now on. Recalla backfills everything you already have, and each memory links to the exact message it came from (`recalla why <id>`).
- **Stale memory is worse than none.** Other tools keep serving a fact after the code it describes changes, with no warning. Recalla anchors memories to code symbols and marks them `needs_reverify` when that symbol changes.
- **Memory shouldn't eat your context.** It uses 4 MCP tools (under 1k tokens), packs are capped at your token budget, and a ledger shows exactly what was injected.
- **Light and private.** It's one binary with no daemon, no LLM, and no network. Secrets are redacted before anything is stored.

## Quickstart

```bash
cargo install recalla          # coming soon
recalla backfill               # import existing Claude Code / Codex sessions
recalla hooks install          # auto-inject context at session start
```

## Features

- Resumable, idempotent backfill with message-level provenance
- Code-anchored memories with soft staleness
- Token-budgeted context packs + injection ledger
- Tiny MCP server: `recall`, `remember`, `review`, `why`
- Layered secret redaction
- Markdown mirror (one file per memory): readable without the engine
- Immutable compressed archive of raw sessions

## Docs

- Spec: SPEC.md · Architecture: ARCHITECTURE.md · Roadmap: ROADMAP.md
- Contributing: CONTRIBUTING.md

## License

Apache-2.0 © 2026 damndeepesh
