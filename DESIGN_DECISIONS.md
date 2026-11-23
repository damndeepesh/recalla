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


## D-019 tree-sitter for symbol maps; one cargo feature per language (2026-11-22)

- Context: Code anchors need the byte range of the definition a memory talks about, so a reformat or an edit elsewhere in the file doesn't flag it stale. Needed for Rust, Python, Go, TS/TSX and JS.
- Decision: Add `tree-sitter` 0.27 (+ `streaming-iterator`, which its `QueryCursor::matches` API requires) and grammars `tree-sitter-{rust 0.24, python 0.25, go 0.25, typescript 0.23, javascript 0.25}`, each optional behind `lang-rust|python|go|ts|js` (all on by default). A language whose feature is off gets path-only anchors. Parsing is capped at 2 MB and 500 ms and returns no symbols on failure. Release binary: 4,810,704 B before, 10,116,464 B after (+5.3 MB, all five grammars), 4,827,792 B with `--no-default-features`.
- Why: Real parsing gets nested items right (methods qualified by their `impl` type or class, nested classes, `const f = () => ...`, items inside macros-free modules) where line regexes break on braces in strings, comments and multi-line signatures. A false "changed" or "removed" erodes trust in the whole staleness signal, which is the product's differentiator. The size cost is real but bounded, and the feature flags let packagers drop languages.
- Rejected: Per-language regex symbol maps (about 0 KB, but wrong on nesting, impl methods and string/comment content, and each new language is another bespoke regex); `ast-grep`/`syntect` wrappers (heavier, same grammars underneath); `git2` for HEAD (C dependency; reading `.git/HEAD` directly is enough for an informational field).

## D-020 Paths and branch names are redacted with layers 1-2 only (2026-11-22)

- Context: Layer 3 (entropy) redacted random-looking path components such as tempdir names, turning `cwd` and so the project key into `[REDACTED:high-entropy]`; anchoring and `check` then silently skipped those projects.
- Decision: `redact_path` runs layers 1 and 2 plus the residual pass (known formats, keyword assignments, URL credentials) but not layer 3. Ingest uses it for `cwd` and `git_branch`; `text` and `tool_name` still use full `redact`.
- Why: Random path components (tempdirs, hashes, build ids) are common and rarely secrets, while known credential formats and `key=value` secrets embedded in a path are still caught.
- Rejected: An allow-list of tempdir shapes (brittle, per-OS); redacting then restoring the project key (leaks the redacted value into the key); skipping redaction for paths entirely (misses tokens pasted into a cwd).

## D-021 Markdown mirror: own YAML subset, versioned format, rebuild in place (2026-11-22)

- Context: SPEC requires that an agent can read memory with no engine, and that the memory layer can be rebuilt from those files. Every memory field (timestamps, status, confidence, `seen_count`, provenance, anchors with `stale_reason`) must survive the round trip, including text with `---`, quotes, colons, multibyte characters and control characters.
- Decision: One `.md` file per memory, `<data>/mirror/{global|projects/<slug>}/<kind>/<uid>.md`, written and parsed by our own code (`mirror::format`): a strict YAML subset (scalars bare or JSON-quoted, one level of lists of maps) followed by the text verbatim. `format: 1` is the first key and the parser refuses any other version, unknown or duplicate keys. Sync is incremental over a dirty set (schema v7 `mirror_dirty(seq, memory_id UNIQUE)`, filled by triggers on memories/anchors/provenance and seeded with every memory on migration; a sync clears only the `seq` marks it read, so a concurrent change stays dirty; no clock dependence), writes only files whose bytes differ, via tmp + rename, 0700 dirs / 0600 files; strays are reported on `--full`, never deleted. Store changes that touch only children (anchors) now bump `memories.updated_at`. `recalla rebuild --from-mirror` parses and verifies every file first (uid must equal `model::uid(kind, scope, text)`, no duplicate uids, no dangling `superseded_by`); any problem refuses the whole rebuild, listing up to 20 files, before anything is touched. Parsing also refuses any string that `redact`/`redact_path` would alter, control or invisible format characters (bidi, zero-width, tag block, variation selectors), and out-of-range values, so a hand-edited mirror cannot inject secrets or prompt-injection text. It then backs the DB up with `VACUUM INTO recalla.db.bak-<unix-secs>[-n]` (target pre-created 0600 with `create_new`, never overwritten) and replaces only the memory-layer tables (memories, provenance, anchors, relations) in ONE `BEGIN IMMEDIATE` transaction on the live DB. The DB file is never swapped, so other connections (`recalla mcp`, hooks) stay valid. `--yes` is refused when the mirror has fewer memories than the DB unless `--allow-drop`. Dry run unless `--yes`; it reports totals, global count, top projects and what would be dropped.
- Superseded (amended before release): the first version copied the DB and renamed the copy over the live file, which corrupts a DB held open by another process; and used an `updated_at` watermark, which freezes under clock skew and races with concurrent writers.
- Why: No new dependency (`serde_yaml` is archived and lenient, and a real YAML parser accepts far more than we ever write, which makes tamper detection and lossless round trips harder to reason about). A subset we render and parse ourselves is small, deterministic (same bytes every time, so sync can compare bytes) and still valid YAML/frontmatter for other tools. The uid check turns a hand-edited text into a refusal instead of silent corruption. Building in a copy and swapping last means a failed or refused rebuild leaves the live database untouched, and events, sources, segments and the injection ledger carry over without being mirrored.
- Rejected: `serde_yaml`/`serde_yml` (dependency, archived, accepts anchors/tags we never want); JSON or TOML frontmatter (less readable for people and agents); one big mirror file (no incremental sync, noisy diffs); deleting strays or files of archived memories (breaks the never-hard-delete rule); rebuilding in place (a crash would leave a half-restored DB); mirroring events (size, and raw transcripts must stay in the archive only).

## D-022 Hand-rolled bounded HTTP/1.1 server (std::net + httparse) for the dashboard (2026-11-23)

- Context: `recalla ui` serves a local dashboard, the first network-facing code in the project. It needs a loopback HTTP server with strict limits and no async runtime.
- Decision: `src/ui/server.rs` on `std::net::TcpListener` with the `httparse` crate (zero dependencies, the parser inside hyper) for request parsing. Binds 127.0.0.1 only, one thread per connection capped at 32 (excess get 503), 10 s overall request deadline, head max 16 KiB, body max 64 KiB with `Content-Length` only, no `Transfer-Encoding`, no `Expect`, no keep-alive (`Connection: close`), GET/HEAD/POST only. Ctrl-C uses default process termination (no signal dependency; the server holds nothing to flush).
- Why: a few hundred lines we fully control and test with attack cases, no TLS or runtime crates, negligible binary size and startup cost.
- Rejected: tiny_http (last release 2022, pulls TLS crates); axum/hyper (tokio runtime: binary size and startup); rouille (2023, depends on tiny_http).

## D-023 Dashboard security hardening after review (2026-11-23)

- Context: the pre-release security review of `recalla ui` (D-022) found the login code reachable through argv and URLs, a session cookie shared across ports, a session that never expired and a response write with no total deadline.
- Decision: login goes through a private redirect page (`<data>/run/login-<random>.html`, 0600 in a 0700 dir) that the browser opener receives as a path, so the code is never in argv; the cookie is named `recalla_<port>`, has no `Secure` flag, and the session expires after 12 h idle; response writes have a 30 s total deadline; "Test recall" calls `pack_preview`, which is read-only and records no injection.
- Why: argv is visible to every local user via `ps`, a cookie name that carries the port stops one run's cookie being accepted by another, an idle expiry bounds a forgotten tab, and the write deadline stops a slow reader pinning a connection thread; a test that wrote to the ledger would pollute the numbers the dashboard shows.
- Rejected: Bearer header or token in the URL (leaks via history, logs and `ps`); `Secure` cookie (Safari rejects it on http://127.0.0.1); cleaning the login page on SIGINT with a signal crate (new dependency for a 60 s file that the next start sweeps anyway).
- Supersedes: the 10 s request deadline in D-022 (now 5 s to read a request, plus the 30 s total write deadline above).

## D-024 SameSite=Lax for the dashboard session cookie (2026-11-23)

- Context: v0.2.0 opens the browser on a local `file://` redirect page (D-023); the redirect chain `file:// -> /?c=<code> -> 302 /` is cross-site from the start, so browsers withheld the `SameSite=Strict` cookie on the final `GET /` and the dashboard answered 401 (reproduced in Chrome for Testing).
- Decision: the session cookie is `SameSite=Lax` (still HttpOnly, Path=/, per-port name, 12 h idle); everything else in D-022/D-023 stands.
- Why: Lax cookies ride top-level GET navigations, which is what the chain is, but never cross-site fetch/XHR/iframes; no GET mutates, and every POST still needs the Host allowlist, a matching Origin and the CSRF header, so a cross-site page can at most load the shell and cannot read a response.
- Rejected: a 200 interstitial with meta refresh instead of the 302 (more moving parts, not needed once Lax works); a plain `http://` URL in argv (leaks the code, see D-023); keeping Strict and printing the link only (breaks the headline "opens your browser" flow).
- Supersedes: the `SameSite=Strict` part of D-022/D-023.

## D-025 Memory benchmark: separate crate, public-repo ground truth, black-box adapters (2026-11-23)

- Context: self-run memory benchmarks are distrusted (vendors dispute each other's LoCoMo scores), and a suite written by Recalla's author would be tuned to Recalla's rules.
- Decision: `bench/` is a standalone cargo crate (`membench`, own `[workspace]`, `publish = false`, not part of `make check`'s dependency graph) that does not link `recalla`. Every tool, Recalla included, is driven as a black box through an adapter (CLI or MCP stdio). Ground truth is mined deterministically from the git history of pinned public repos (no LLM): causal/decision sentences from commit messages are the facts, a later commit touching the same files is the task, and a fact is stale when the lines its commit added no longer survive. Suites are data (`bench/suites/*.json`) with a tuning split and a held-out split that the extractor is never tuned against. Scoring and weights are fixed in `bench/SCORING.md` before results are published. Dataset building (`fetch`) uses the network; running a suite is offline.
- Why: ground truth that comes from other people's repos cannot be fitted to our extractor; a separate crate keeps the benchmark neutral enough to move to its own repo and keeps `recalla` free of benchmark dependencies.
- Rejected: a hand-written synthetic suite as the headline (author bias; kept only as a smoke test); an LLM judge (non-reproducible, costs money); linking the `recalla` crate for the Recalla adapter (would give it a path other tools do not have).
