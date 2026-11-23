# Plan: memory benchmark (bench/)

## Goal
A neutral, runnable benchmark for coding-agent memory tools. Primary axis: what a good memory gives an agent. Extraction quality and token savings are equal-weight add-ons. Deterministic and offline by default. Recalla is just one adapter, with no special path.

## Design
- Separate cargo workspace member `bench/` (crate `membench`, `publish = false`, MIT/Apache like the repo). Does not link `recalla`; every tool, Recalla included, is driven as a black box through an adapter.
- Adapters (trait `Adapter`): `recalla` (CLI + `recalla mcp`), `mcp` (any MCP stdio server, config = command + tool names for store/recall), `file` (CLAUDE.md-style plain-files baseline, truncated to budget), `none`, `dump` (everything, no budget).
- Two feed modes, reported separately:
  - facts-in: same ground-truth statements written through each tool's write interface (tests retrieval, noise, staleness).
  - transcript-in: synthetic sessions fed to tools that can ingest/extract (tests extraction). Tools without ingestion are marked n/a, not zero.
- Scenario suite = data, not code: `bench/suites/<name>/` with a synthetic git repo generator (seeded), a history of facts/sessions, and probes (task query -> required fact ids, distractor ids, kind).
- Scores (each 0-1, report per axis and a transparent weighted total, equal weights by default):
  1. recall@budget: required facts present in what the tool returns within N tokens
  2. noise: 1 - share of returned tokens not tied to the task
  3. staleness: after a seeded git commit changes anchored code, stale fact is flagged/dropped (not served as true)
  4. provenance: returned item links back to its source
  5. extraction (add-on): precision/recall from transcripts vs labels
  6. token savings (add-on): tokens returned vs dump baseline and vs raw history at fixed recall
- Real-data mode: `membench real --history ~/.claude --repo PATH` builds probes from the user's own sessions/commits (facts found in history with later git evidence). Local only, never writes outside a scratch dir, report redacted.
- Credibility: fixed seeds, pinned token counter (same est_tokens for all, documented), raw per-probe outputs saved, report JSON + markdown, loses-where listed automatically. No network, no LLM, no API key. Agent-run tier (repeat-mistake, adherence) deferred; needs owner approval for subscription use.

## Steps
- [ ] 1. DESIGN_DECISIONS entry (separate crate, black-box adapters, scoring, tokens) + SPEC pointer
- [ ] 2. Workspace + `bench/` crate skeleton, CLI (`run`, `list`, `report`), JSON report types
- [ ] 3. Suite format + seeded synthetic repo/history generator (git commits, decisions/gotchas/preferences, distractors)
- [ ] 4. Adapters: none, dump, file
- [ ] 5. Adapter: recalla (CLI + mcp), MCP-generic client (JSON-RPC stdio)
- [ ] 6. Scorers 1-4 + tests on tiny fixtures with hand-computed expectations
- [ ] 7. Staleness scenario (git commit changes anchored code)
- [ ] 8. Add-ons: extraction (transcript-in), token savings
- [ ] 9. Report (JSON + markdown, per-probe raw, weighted total, where-each-tool-loses)
- [ ] 10. Real-data mode
- [ ] 11. `make bench`, CI smoke (tiny suite), docs (bench/README.md, README link), CHANGELOG, state

## Files
bench/**, Cargo.toml (workspace), Makefile, DESIGN_DECISIONS.md, CHANGELOG.md, .agent/state.yaml, README.md, CI workflow (smoke).

## Reused
tests/fixtures/extract/sessions.json labels (extraction suite seed), est_tokens semantics from src/pack.rs (re-implemented in bench, documented), `recalla` binary as a black box.

## Risks
- Fairness: Recalla authors write the suite. Mitigate: published seeds/raw outputs, plain-files and dump baselines, losses reported, open submissions.
- Other tools' write/recall interfaces differ; MCP-generic adapter needs per-tool config. Start with Recalla + baselines + one MCP tool (basic-memory, files + MCP).
- Staleness only meaningful for tools that track code; others scored as "never flags" (honest, not n/a).
- Scope: keep deterministic tier first; real-data mode last.

## Test plan
Unit tests per scorer with hand-computed fixtures; adapter tests against a fake MCP server; golden JSON report for a tiny suite; `make bench` smoke in CI; `make check` green.
