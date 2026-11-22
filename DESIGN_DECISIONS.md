# Design Decisions

Append-only. To change one, add a new entry that says "Supersedes D-NNN".

## D-001 Rust, single daemonless binary (2026-11-21)

- Context: Hooks run on every agent session start and prompt, so startup and memory dominate. The main competitors' biggest complaints are daemons, CPU spikes and process leaks (claude-mem: Bun + Chroma; ai-memory: always-on server).
- Decision: Rust, one binary, short-lived processes. The only long-lived mode is the MCP stdio server.
- Why: millisecond startup, low RAM, fast bulk JSONL/zstd parsing, a mature tree-sitter + rusqlite ecosystem, single-binary distribution.
- Rejected: Go (weaker tree-sitter bindings, SQLite via cgo); TS/Bun (heavier runtime, the claude-mem resource trap).

## D-002 SQLite + FTS5 as store, markdown mirror as portable format (2026-11-21)

- Context: Retrieval needs to be fast and indexed, but the user wants a format agents can read without the engine.
- Decision: SQLite (WAL) + FTS5 BM25 is the source of truth, with a one-`.md`-per-memory mirror that can rebuild the DB. Vectors are optional, later.
- Why: zero-service, embedded, proven; BM25 is strong for code terms; the mirror gives portability and git-friendliness.
- Rejected: vector DB / graph DB (heavy, opaque); markdown-only (slow queries, no ledger or anchors).

## D-003 Atomic memories with message-level provenance (2026-11-21)

- Context: ai-memory uses pages with coarse provenance. ICM's import loses its source. Users need "why does the agent believe this?".
- Decision: The unit is an atomic memory (kind, text, confidence, importance, scope), with provenance `(harness, session_id, message_uuid, file, line, commit)` or an explicit `remember` origin.
- Why: auditability, precise supersede/dedup, a wedge ai-memory can't copy without re-architecting.
- Rejected: page/session summaries (coarse); LLM-written observations (cost, nondeterminism).

## D-004 Rule-based extraction by default, no LLM (2026-11-21)

- Context: LLM observers cost about 64% extra spend (claude-mem #3848), and mem0 audits found mostly junk.
- Decision: Deterministic rule and heuristic extraction, measured against a labeled eval set. A local-LLM pass is a later opt-in feature.
- Why: offline, free, reproducible, testable.
- Rejected: required cloud LLM; default to the user's agent CLI (ICM's CPU-loop bug #322).

## D-005 Apache-2.0, no monetization in MVP (2026-11-21)

- Context: The goal is adoption. Competitors use MIT or Apache. A hosted team-sync tier may come later.
- Decision: Apache-2.0.
- Why: permissive for adoption, adds a patent grant, compatible with open core later.
- Rejected: MIT (no patent grant); AGPL (deters adoption and integrations).

## D-006 Core store dependencies: rusqlite, thiserror, blake3 (2026-11-22)

- Context: The core model + store needs an embedded DB with FTS5, typed errors, and deterministic ids; hooks, CLI and MCP run as concurrent processes.
- Decision: `rusqlite` (feature `bundled`, so FTS5 is always present), `thiserror` 2 (typed `StoreError`), `blake3` (deterministic content `uid`: idempotent re-imports); dev-only `tempfile` (file-backed DB tests). Migrations are a hand-rolled `PRAGMA user_version` list.
- Why: SQLite WAL handles multi-process access; BM25 comes from FTS5 with no extra index; blake3 is fast, tiny, and needs no id coordination across machines.
- Rejected: redb (single-process lock); Tantivy (single writer, heavier; maybe later as an optional search booster); rusqlite_migration (hand-rolled user_version is enough); uuid/ulid crates (random ids break idempotent re-imports).

## D-007 Secret redaction with the `regex` crate and curated rules (2026-11-22)

- Context: Every transcript passes `redact` before persist. Transcripts are untrusted input, so matching must be offline, deterministic, and immune to pathological inputs.
- Decision: `regex` 1 (patterns compiled once via `std::sync::LazyLock`), with three layers: curated known-format rules, keyword-assignment values, and a Shannon-entropy catch-all with hex/UUID/path/identifier exclusions. Rules are original regexes, not copied from GPL sources; any pattern adapted from gitleaks (MIT) carries a credit comment.
- Why: `regex` guarantees linear-time matching, so no ReDoS from untrusted transcripts; it is maintained by the rust-lang project; its only deps are its own `regex-syntax`/`regex-automata` (plus `aho-corasick`, `memchr`).
- Rejected: fancy-regex / pcre2 (backtracking engines, ReDoS risk); shelling out to gitleaks/trufflehog (external process, Go and heavy, not embeddable in a single binary); vendoring the full gitleaks ruleset (too broad and noisy for transcripts; a curated set plus entropy fits better).

## D-008 Redaction errs toward over-redaction (2026-11-22)

- Context: Security review found 8 bypasses, all caused by exemptions that were too generous: a code heuristic, a substring placeholder match, ID prefixes, stop chars that split values, header schemes, and URL `@` handling.
- Decision: Exemptions must match the WHOLE value or an exact shape. Unquoted values run to whitespace. Any key whose word ends in a secret word counts. Any Authorization/Cookie value is redacted regardless of scheme. URL passwords run to the last `@`.
- Why: A leaked secret is irreversible; an over-redacted word is cheap. The raw archive keeps the original if the user ever needs it.
- Rejected: Precise-but-permissive heuristics (each one was a bypass); an LLM classifier (offline/no-LLM invariant).

## D-009 Redaction structure: no hand-offs, residual pass, decode before redact (2026-11-22)

- Context: Two more review rounds found bypasses where one layer skipped text assuming another would catch it, where caps failed open, or where JSON escapes split tokens.
- Decision: Layers never defer; all emit and `merge` unions. A residual pass redacts any recognised secret key/header value that no finding covers. Size limits fail closed (layer-2 values redact the whole run). Harness parsers JSON-decode strings before `redact`; escape tolerance in `redact` is defense in depth only.
- Why: Makes "key recognised but value exempted" impossible by construction, and puts decoding at the one layer that knows the format.
- Rejected: Per-case patches (each produced new bypasses); a JSON unescaper inside `redact` (wrong layer, duplicates harness parsing).

## D-010 Realistic corpus as the redaction gate; fixpoint rescan (2026-11-22)

- Context: Rule-probing reviews kept finding edge cases, but the final gate found 4 HIGH leaks in plain real-world lines (`*_KEY=<hex>`, `curl -u`, `.netrc`, `*_PAT`). Also, rare crafted inputs broke idempotence.
- Decision: `tests/realistic_corpus.rs` (108 real-world credential templates × 20 runtime secrets) is the merge gate for any redaction change. `redact()` reruns detection on its own output (≤4 extra passes) and maps new findings back, which makes idempotence hold by construction.
- Why: Testing what developers actually write catches whole missing formats. The fixpoint costs ~45% throughput (82 to 46 MB/s, still ~7 s per 300 MB) for a guarantee instead of a heuristic.
- Rejected: More rule-probing rounds as the only gate (diminishing returns); targeted idempotence patches only (fuzz still found failures).

## D-011 gitleaks in CI replaces push protection (2026-11-22)

- Context: Making the repo private turned off GitHub secret scanning and push protection (needs paid GHAS). GitGuardian still comments on PRs but doesn't block.
- Decision: A `secrets` CI job runs `gitleaks/gitleaks-action@v2` over full history; `make secrets` runs the same locally. `.gitleaks.toml` extends the default rules with one allowlist: matches containing runtime template placeholders (`{S}`, `{pw}`, ...). Files are never ignored wholesale.
- Why: Free for personal repos, blocks merges on a leak, and catches generic `password=` literals our vendor-format grep missed.
- Rejected: Path allowlists for tests/ (would hide real leaks in fixtures); relying on GitGuardian alone (non-blocking).

## D-012 serde_json with lenient Value parsing for harness logs (2026-11-22)

- Context: Claude Code session logs are JSONL in an external format that changes between versions. The harness must decode every line.
- Decision: Add `serde_json = "1"` (no serde derive in our code). Parse each line into `serde_json::Value` and read fields through accessors; missing or unknown fields are counted in `ParseStats`, never fatal.
- Why: Decoding is mandatory (D-009) and serde_json is the de-facto standard, pure Rust. `Value` tolerates schema drift where typed structs would reject whole records.
- Rejected: Typed serde structs (brittle against an external, versioned format); simd-json (unneeded complexity for MB-scale files).

## D-013 clap (derive) for the CLI parser (2026-11-22)

- Context: The CLI grows to many subcommands (`backfill`, hooks, `mcp`, `why`, ...) with flags, help text and exit-code conventions.
- Decision: `clap = { version = "4", features = ["derive"] }`; argument structs live in `cli`, `main.rs` only calls `cli::run`.
- Why: The de-facto Rust standard; derive keeps flags, help and validation in one typed place; generated `--help`/`--version` and usage errors (exit 2) for free.
- Rejected: lexopt / argh (smaller, but hand-written help and poorer DX across many subcommands); hand-rolled parsing (already outgrown).

## D-014 zstd for the raw session archive (2026-11-22)

- Context: The archive keeps an immutable copy of raw session bytes; JSONL logs are large and highly repetitive.
- Decision: `zstd = "0.13"` (libzstd bindings), level 3, one content-addressed `<blake3>.zst` per ingested segment.
- Why: Logs compress about 10x; zstd is fast at both ends and the C library is the reference implementation.
- Rejected: ruzstd (pure-Rust encoder is slow and immature); gzip/flate2 (worse ratio and speed); no compression (about 10x more disk).

## D-015 Tool-input JSON is redacted per string leaf (2026-11-22)

- Context: Tool inputs are stored as compact JSON. Redacting the escaped string missed some secrets (e.g. `password=\t<value>`), and the extractor later decoded them into memories. Truncating the serialized JSON also broke it, so large ADR writes were silently skipped.
- Decision: Ingest parses ToolCall JSON, redacts every string leaf, re-serializes, then runs the whole-string redact again for key context. Oversized inputs shrink leaves first, so the JSON stays valid. The extractor also re-redacts every JSON-decoded string as a backstop.
- Why: It completes D-009 (decode before redact) for structured data, and memories are the agent-facing layer, so they get two independent guards.
- Rejected: Whole-string redaction only (escaped forms leak); redacting object keys (they are tool schema names, not secrets).

## D-016 Token budget enforced on a conservative byte estimate (2026-11-22)

- Context: Packs must never exceed a token budget, and hooks run them on every session start, so the count must be fast, offline and dependency-free. The target model's tokenizer isn't public or stable.
- Decision: `est_tokens(s) = ceil(utf8_bytes / 3) + 2 per line` (lines = `\n` count + 1). The budget is enforced on this estimate over the whole rendered text, header included, so the real count is lower.
- Why: Claude averages about 3.5-4 bytes per token on English and code, so bytes/3 overestimates by 15-30%. CJK is roughly 1 token per 3-byte char, so it is close to exact, and emoji (4 bytes, 1-3 tokens) are over-counted. The per-line term covers newline and markdown-marker tokens. A wrong estimate wastes some budget but can't overflow it.
- Rejected: A tokenizer crate (heavy dependency, wrong vocabulary for Claude, slower startup); chars/4 (underestimates CJK and dense code, so it can overflow); a feature-flagged exact tokenizer (possible later, not needed for the MVP).

## D-017 Hand-rolled sync stdio JSON-RPC for MCP, serving both protocol eras (2026-11-22)

- Context: `recalla mcp` exposes 4 tools to Claude Code, Codex and other MCP clients. Clients in the wild speak either the 2025-11-25 `initialize` handshake or the stateless 2026-07-28 revision (version in every request's `_meta`, `server/discover`).
- Decision: `src/mcp/` is a synchronous loop over newline-delimited JSON on stdin/stdout, no new dependencies (serde_json only). One request at a time, store opened per call. Both eras are served by detecting `params._meta["io.modelcontextprotocol/protocolVersion"]`: modern results add `resultType:"complete"`, legacy ones omit it; unknown versions get -32022 with `{supported, requested}`. Notifications are never answered.
- Why: 4 request/response tools need no concurrency, streaming or subscriptions. Startup stays instant and the binary small. The protocol subset we use is about 150 lines and is covered by scripted-stream and child-process tests. Dual-era support avoids breaking older clients while newer ones work without a handshake.
- Rejected: `rmcp` crate (pulls in a tokio runtime, bigger binary and slower startup, overkill for 4 tools, and its tracking of two protocol eras is not guaranteed); 2026-only (breaks current clients); legacy-only (dead end as clients move to stateless).

## D-018 Hooks never block the session; settings edits are conservative (2026-11-22)

- Context: `recalla hook session-start|capture` run inside Claude Code on every session start, Stop and PreCompact. `recalla hooks install` edits the user's own `settings.json`.
- Decision: hooks always exit 0 and never write errors to stdout or stderr; failures and panics (`catch_unwind`, silent panic hook) append one content-free line to `<data>/log/hook.log` (dir 0700, file 0600, cut to the last 512 KB past 1 MB). `capture` prints nothing; `session-start` prints one JSON line or nothing. Hook SQLite connections use a 2 s busy timeout, then give up. `capture` ingests only `transcript_path`, and only if it canonicalizes under `<claude root>/projects`, so stdin cannot name arbitrary files. The source key is that canonical path, identical to a full backfill's. `install` refuses a settings file that is malformed or not a JSON object (exit 1, nothing written), backs up to `settings.json.recalla-bak-<unix-ts>` before any write, writes atomically (tmp + fsync + rename, mode kept, symlinks followed), updates our entries in place (idempotent) and never touches other keys or hooks. `uninstall` removes only our entries and prunes groups/arrays it emptied. MCP registration is printed, never run.
- Why: a memory tool must not be able to break or slow a coding session, and `settings.json` holds the user's other hooks and permissions. serde_json's `preserve_order` feature (pulls `indexmap`) keeps key order so users' diffs stay small and the hook JSON has the documented key order.
- Rejected: non-zero exit or stderr on error (shown to the model or blocks the session); editing settings through string splicing (fragile); a JSON-with-comments parser (Claude Code settings are plain JSON); skipping backups for no-op installs is intentional (nothing is written).

