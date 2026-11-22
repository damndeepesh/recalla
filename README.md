<div align="center">

# Recalla

**Memory your coding agents can trust.**

Local-first memory engine for Claude Code, Codex and other coding agents.
It turns your existing session history into compact, source-linked memories, flags a memory when the code it describes changes, and gives agents only what fits a token budget you set.

[![CI](https://github.com/damndeepesh/recalla/actions/workflows/ci.yml/badge.svg)](https://github.com/damndeepesh/recalla/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/rust-stable-orange.svg)
![Status](https://img.shields.io/badge/status-alpha-yellow.svg)

</div>

> [!WARNING]
> **Alpha (v0.1.0).** Imports Claude Code and Codex history and works end to end with Claude Code: backfill your history, install the hooks, and every new session starts with relevant memory. Extraction is rule-based, precise but not exhaustive, and still being tuned on real histories. See [ROADMAP.md](ROADMAP.md) for what's next, and please [open an issue](https://github.com/damndeepesh/recalla/issues) if it misses or invents something on your logs.

---

## Why Recalla

Coding agents forget everything between sessions, so you re-explain decisions and they repeat debugging they've already done. Memory tools exist, but they share four problems:

| Problem                                                                                                                               | What Recalla does                                                                                                                                      |
| ------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **History you already have is ignored.** Most tools only capture from install onward.                                                 | **Backfills** existing Claude Code and Codex sessions into atomic memories. Every memory links to the exact message it came from (`recalla why <id>`). |
| **Stale memories are served with full confidence.** A fact about code that has since changed looks as true as the day it was written. | **Anchors** memories to code symbols. When the symbol changes, the memory is marked `needs_reverify` instead of being silently trusted.                |
| **Memory eats the context window.** Large tool surfaces and LLM "observers" add thousands of tokens per request.                      | **Token budgets:** a 4-tool MCP surface (< 1k tokens), packs capped at the budget you set, and a ledger of exactly what was injected.                  |
| **Heavy runtimes and privacy risk.** Background daemons, vector databases and cloud extraction.                                       | **One binary, no daemon, no LLM, no network.** Secrets are redacted before anything touches disk.                                                      |

## How it works

```mermaid
flowchart LR
  S[Agent session logs] --> P[Parse + normalize]
  P --> R[Redact secrets]
  R --> X[Extract atomic memories]
  X --> DB[(SQLite + FTS5)]
  P --> A[Compressed raw archive]
  C[Your code] --> AN[Symbol anchors] --> DB
  DB --> K[Token-budgeted context pack] --> G[Agent: hooks / CLI / MCP]
```

- **Atomic memories:** decisions, facts, procedures, gotchas, preferences and tasks, each with provenance, confidence, scope (global, project or module) and a reversible lifecycle. Nothing is hard-deleted.
- **Deterministic identity:** a memory's ID is a hash of its content, so re-importing the same history never creates duplicates, and two machines agree on IDs without syncing.
- **Readable without the engine:** a markdown mirror (one file per memory) lets any agent or person read memory with no tooling.

See [ARCHITECTURE.md](ARCHITECTURE.md) for components and data flow.

## Status

Alpha (v0.1.0). Everything in the MVP is built and covered by tests and an end-to-end script.

| Component | State |
| --- | --- |
| Memory model + SQLite store (dedup, supersede/restore, BM25 search) | ✅ |
| Layered secret redaction (known formats, keyword values, entropy) | ✅ |
| Claude Code and Codex session parsers, resumable backfill, raw archive | ✅ |
| Rule-based extraction (preferences, decisions, gotchas, procedures, facts, tasks) | ✅ |
| Code anchors + staleness (Rust, Python, Go, TypeScript, JavaScript) | ✅ |
| Token-budgeted context packs + injection ledger | ✅ |
| MCP server (4 tools) + Claude Code hooks | ✅ |
| Markdown mirror + `rebuild` | ✅ |
| Gemini CLI / OpenCode / Cursor sessions, contradiction resolution, embeddings | 🗺️ planned, see [ROADMAP.md](ROADMAP.md) |

## Getting started

```bash
cargo install recalla
# or from source:
cargo install --git https://github.com/damndeepesh/recalla
```

Needs a stable Rust toolchain. macOS and Linux are tested in CI; Windows is untested.

Then, in your terminal:

```bash
recalla backfill        # import your Claude Code + Codex history and extract memories
recalla hooks install   # new Claude Code sessions start with relevant memory (backs up settings.json first)
```

Restart Claude Code and open a project you have history for. `recalla pack` shows what would be injected.

To work from a checkout instead: `git clone https://github.com/damndeepesh/recalla && cd recalla && make check`.

## Numbers

Measured on an Apple M2, reproducible from a checkout:

| What | Result | How |
| --- | --- | --- |
| Backfill of a 320 MB synthetic Claude + Codex history | 21.6 s (re-run: 0.03 s) | `make perf` |
| Session-start hook with 100k memories | p95 13 ms | `make perf` |
| MCP tool descriptions | about 430 tokens for all 4 tools | `make e2e` |
| Binary size | 10 MB (each language grammar is a cargo feature) | `cargo build --release` |
| Extraction quality | precision 1.00, recall 0.92 on a hand-labeled **synthetic** eval (53 labels). Treat this as a regression gate, not a claim about your logs. | `cargo test --test extract_eval` |

`make e2e` runs 64 end-to-end checks on the release binary: planted secrets never reach the database, hooks never touch your other settings, MCP works in both protocol versions, anchors flag the right function, and the mirror rebuilds identically.

## Known limits

- **Extraction is rule-based.** It is precise but misses implicit rules, and it can still pick up a stray sentence. Check what it found with `recalla search`. To drop a wrong memory, ask your agent to forget it through the MCP `review` tool (reversible with `recalla confirm <id>`); a standalone `recalla forget` command is the first post-release item. Please [open an issue](https://github.com/damndeepesh/recalla/issues) for anything it gets wrong.
- **Anchors need code to anchor to.** A memory is anchored when it names a symbol that exists in a file from the same session (or a file path in its text). Prose-only histories produce few anchors.
- **Claude Code gets the hooks.** Codex is covered by `backfill` and by MCP; native Codex hooks are on the roadmap.
- **English only** for extraction phrases.

## Commands

```bash
recalla backfill              # import Claude Code + Codex history and extract memories
recalla hooks install         # inject memory at session start, capture new turns (backs up settings.json)
# optional: expose recall/remember/review/why as MCP tools
claude mcp add --scope user recalla -- "$(command -v recalla)" mcp

recalla pack --budget 1500    # see the context pack for the current project
recalla search "rate limit"   # inspect memories
recalla why <id>              # where a memory came from
recalla stats                 # what was injected, and its token cost
recalla check                 # flag memories whose anchored code changed
recalla confirm <id>          # re-verify a flagged memory
recalla hooks uninstall       # remove only Recalla's hook entries

recalla mirror [--full]       # sync the markdown mirror now (it also updates after backfill, hooks and edits)
recalla rebuild --from-mirror # restore memories from the mirror; dry run first, --yes to apply (a backup is made)
```

### The markdown mirror

Every memory is also written as a plain markdown file under `<data dir>/mirror/` (default `~/.local/share/recalla/mirror/`), in `global/` and `projects/<project>/<kind>/<id>.md`. Each file has a small frontmatter (kind, scope, status, confidence, provenance, anchors) and the memory text. You or any agent can read it with `cat` and `grep`, no Recalla needed. Files are private (0600) and never deleted: archived and superseded memories keep theirs.

If the database is lost or damaged, `recalla rebuild --from-mirror` restores the memory layer from these files. Events and sources are kept. It is safe to run while Claude Code or `recalla mcp` are running: it takes a `VACUUM INTO` backup (`recalla.db.bak-<time>`) and then replaces the memory layer in one transaction, never swapping the database file.

It checks before touching anything and refuses the whole rebuild if any file is malformed, was edited (its id no longer matches its text), contains unredacted secrets, control or invisible characters, or has out-of-range values. The dry run shows how many memories are global, how many belong to each project, and which memories in the database are missing from the mirror; removing those needs `--yes --allow-drop`. Format: [ARCHITECTURE.md](ARCHITECTURE.md#mirror-mirror-recalla-mirrorrebuild).

## Privacy and security

- **Local only.** No network calls and no LLM calls in the default build. Your history never leaves your machine.
- **Redact before persist.** Secrets are scrubbed in three layers before any text is stored:
  1. Known credential formats
  2. Values of secret-bearing keys, headers and CLI flags
  3. An entropy catch-all for unknown formats

  The redactor is tested against a corpus of real-world credential shapes and fuzzed for crashes and idempotence.

- **Source logs are never modified.**

To report a vulnerability, see [SECURITY.md](SECURITY.md). Please don't open a public issue.

## Design principles

- **Trust over recall:** a stale or unverifiable memory is worse than none.
- **Measured, not claimed:** every number in this README comes from a command you can run (`make perf`, `make e2e`, the eval). Known weak spots are listed above, not hidden.
- **Small surface:** few dependencies and few MCP tools. Every dependency is justified in [DESIGN_DECISIONS.md](DESIGN_DECISIONS.md).

## Contributing

Contributions are welcome. Start with [CONTRIBUTING.md](CONTRIBUTING.md). Issues labelled [`redaction`](https://github.com/damndeepesh/recalla/labels/redaction) are a good place to begin. All contributors are expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
