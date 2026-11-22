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
