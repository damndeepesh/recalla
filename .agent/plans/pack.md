# Plan: context packs + injection ledger (`recalla pack`, `stats`, `why`)

## Goal

Turn stored memories into a compact, token-budgeted context pack for the current project. Packs must never exceed the budget, and every injection is logged in a ledger so users can see what memory costs them. This is the piece that lets agents actually use Recalla (hooks and MCP wrap it next).

## Design

- **Project detection:** `--project` overrides; otherwise `extract::scope::project_key(cwd)`, using the same key the extractor stores.
- **Candidates:** active and needs_reverify memories in scope `Global` ∪ `Project(key)` ∪ `Module{project: key, ..}`. New store API: `memories_for_project(key, statuses)`.
- **Ranking** is deterministic. The score is `kind_weight × confidence × (1 + ln(seen_count)) × recency`.
  - `kind_weight`: preference 1.0, gotcha 0.9, decision 0.85, procedure 0.7, task 0.6, fact 0.5. Constraints and warnings come first, per SPEC.
  - `recency` = `0.5 + 0.5 × exp(-age_days / 90)`, based on `updated_at`.
  - With `--query Q`: candidates that are also BM25 hits get ×(1.5 + normalized rank). Other candidates keep their score.
  - Ties break on uid, so output is stable.
- **Token estimate:** no tokenizer dependency. `est_tokens(s) = ceil(utf8_bytes / 3)` plus 2 per line. This deliberately overestimates: Claude averages ~3.5–4 chars/token for English and code, and CJK is ~1 token per 3-byte char. The budget is enforced on the estimate, so the real count is always lower. The rationale is documented in an ADR. Header and footer cost is included.
- **Greedy fill:** in rank order, add each memory if it fits. Skip, don't stop, so smaller later items can still fit. Sections are kept in a fixed order for readability.
- **Format:** markdown, terse, agent-readable.
  ```
  # Recalla memory · project recalla · 412/2000 tok
  ## Preferences
  - Never touch the magurelabs org. [a1b2c3d4]
  ## Gotchas
  ## Decisions
  ## Procedures
  - `make check` (15×) [e5f6…]
  ## Needs re-verify ⚠
  ```
  `--format json` emits structured output for MCP.
- **Ledger:** migration v4 adds `injections(id, at, project, budget, est_tokens, memory_uids TEXT /*json array*/, via TEXT /*cli|hook|mcp*/)`. `pack` records a row unless `--no-log` is passed. `recalla stats` shows:
  - injections count
  - total and average estimated tokens
  - per-project breakdown
  - most-injected memories
- **`recalla why <uid-prefix>`** prints:
  - the memory: kind, text, confidence, seen_count, status, scope
  - every provenance row: harness, session prefix, source file, line
  - the stored (redacted) source event text for each provenance, looked up by (source path, line) in `events`
  - anchors

  An ambiguous prefix lists the matches and exits 2.

## Steps

- [x] 1. Migration v4 (injections) + store APIs: memories_for_project, record_injection, injection_stats, memory_by_prefix, events_at(source_path, line)
- [x] 2. `pack` module: token estimator, scorer, greedy fill, markdown/json render (pure, unit-tested)
- [x] 3. CLI: `pack [--budget N=1500] [--project P] [--query Q] [--format md|json] [--no-log]`, `stats [--json]`, `why <id>`
- [x] 4. Tests:
  - property-style: 500 random memory sets × random budgets never exceed the budget, including multibyte text
  - ranking order
  - project union excludes other projects
  - needs_reverify section is labeled
  - ledger rows recorded
  - `why` shows provenance + source text
  - ambiguous prefix
- [x] 5. Real-data smoke on the dev DB
- [x] 6. Docs (ARCHITECTURE pack + ledger, ADR token estimate, README status/usage, AGENTS commands)

## Risks

- **Token overestimation wastes budget:** acceptable, since it's safer than overflow. A real tokenizer can be added later behind a feature flag.
- **Project key mismatch** between extraction-time cwd and pack-time cwd (subdirectories): both resolve to the git toplevel.
- **The ledger grows unbounded:** small rows. Add `stats --prune-older-than` later.
