# Plan: core model + store

## Goal

Typed memory model plus a SQLite store: the foundation every other module writes to and reads from. Covers schema, migrations, FTS5/BM25 search, dedup on insert, and reversible supersede/archive. No extraction, anchors logic, or CLI yet.

## Design

- **Identity:** `uid` = first 16 bytes (hex) of `blake3(kind | scope | normalized_text)`.
  - Deterministic, so re-imports are idempotent and machines agree on ids without coordination.
  - `id` INTEGER is internal only.
- **Normalization for the hash:** trim + collapse whitespace. Case preserved (identifiers matter).
- **Dedup:**
  - Same `uid` → no new memory. Its provenance row is added (unique on source key), `seen_count++`, and confidence becomes `max(old, new)`.
  - Returns `Insert::Duplicate(uid)`.
- **Status:** `active | needs_reverify | superseded | archived`.
  - Never hard-deleted.
  - `restore` returns an archived or superseded memory to active, and clears `superseded_by` on its successor link.
- **Supersede:** `supersede(old, new)` sets old → `superseded`, `superseded_by=new`, `valid_to=now`, in one transaction.
- **Provenance:** 1..n rows per memory.
  - Fields: `origin` (backfill|capture|remember), `harness`, `session_id`, `message_uuid`, `source_path`, `line`, `git_commit`.
  - Unique on (memory, harness, session, message, line), so idempotent.
- **Anchors:** table and type only (path, symbol, lang, content_hash, commit). Staleness logic lands in the anchors TODO item.
- **FTS5:** an external-content table over `memories.text` + `kind`, kept in sync by triggers. Search returns BM25 rank and filters by scope/kind/status. Default excludes superseded/archived.
- **Migrations:** hand-rolled, ordered SQL list + `PRAGMA user_version`, run inside a transaction on open.
- **Pragmas:** WAL, `foreign_keys=ON`, `busy_timeout=5000`.
- **Time:** unix millis `i64` from `std::time`. A `Clock` is passed in for tests (no deps).
- **Errors:** `thiserror` `StoreError`; no `unwrap` outside tests.

## Steps

- [x] 1. Add deps (rusqlite[bundled], thiserror, blake3; dev: tempfile) + DESIGN_DECISIONS D-006
- [x] 2. `model`: `Kind`, `Scope`, `Status`, `Origin`, `Memory`, `NewMemory`, `Provenance`, `Anchor`, `uid()` + normalization, with unit tests
- [x] 3. `store::schema`: migrations v1 (memories, provenance, anchors, relations, memories_fts + triggers) + `Store::open`/`open_in_memory`, pragmas, and a test that migrating twice is a no-op
- [x] 4. `store` writes: `insert` with dedup + provenance merge, `get`, `supersede`, `archive`, `restore`, `set_status`, all transactional; tests
- [x] 5. `store` search: `search(query, Filter, limit)`, BM25-ranked, FTS query escaping, scope/kind/status filters; tests
- [x] 6. Integration test (`tests/store.rs`): file-backed DB, reopen persists, dedup across reopen, supersede/restore round-trip
- [x] 7. Update ARCHITECTURE (schema summary), TODO tick, state.yaml, CHANGELOG

## Files touched

- `Cargo.toml`, `Cargo.lock`
- `src/lib.rs`, `src/model.rs`, `src/store/{mod.rs,schema.rs,search.rs}`
- `tests/store.rs`
- DESIGN_DECISIONS.md, ARCHITECTURE.md, TODO.md, CHANGELOG.md, `.agent/state.yaml`

## Reused modules

- None yet (first feature). `version_line` stays.

## Risks

- **FTS5 query syntax injection or crash on user text** (quotes, `*`, `NEAR`). Mitigation: tokenize the input and quote each term; fuzz-ish tests with punctuation.
- **External-content FTS drift** if triggers are wrong. Mitigation: a test that updates and supersedes rows, then checks FTS hits; `rebuild` helper.
- **Hash collision on a 128-bit uid:** negligible. Documented.
- **Normalization too weak** (near-duplicates slip through). Acceptable for now; near-dup (SimHash) is later.
- **WAL with concurrent hook processes.** `busy_timeout` plus short transactions; a concurrency test comes in the perf item.

## Test plan

- Unit tests:
  - uid stability and normalization
  - Kind/Status string round-trip
- Store tests:
  - insert → get
  - duplicate insert merges provenance
  - provenance idempotent on the same source
  - supersede hides the old memory from default search but not from `get`
  - restore round-trip
  - archive excluded unless filtered in
- Search tests:
  - BM25 ranks the more relevant memory higher
  - Punctuation and FTS operators in the query don't error
  - Scope filter isolates projects
- Migrations: open twice gives the same user_version, and no error.
- `make check` green.
