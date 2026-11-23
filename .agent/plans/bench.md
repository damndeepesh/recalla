# Plan: memory benchmark (bench/)

## Goal
A neutral, runnable benchmark for coding-agent memory tools. Primary axis: what a good memory gives an agent. Extraction quality and token savings are equal-weight add-ons. Deterministic and offline by default. Recalla is just one adapter, with no special path.

## Design
- Separate cargo workspace member `bench/` (crate `membench`, `publish = false`, MIT/Apache like the repo). Does not link `recalla`; every tool, Recalla included, is driven as a black box through an adapter.
- Adapters (trait `Adapter`): `recalla` (CLI + `recalla mcp`), `mcp` (any MCP stdio server, config = command + tool names for store/recall), `file` (CLAUDE.md-style plain-files baseline, truncated to budget), `none`, `dump` (everything, no budget).
- Two feed modes, reported separately:
  - facts-in: same ground-truth statements written through each tool's write interface (tests retrieval, noise, staleness).
  - transcript-in: synthetic sessions fed to tools that can ingest/extract (tests extraction). Tools without ingestion are marked n/a, not zero.
- Ground truth from public data (D-025): `membench mine` turns pinned public repos (ripgrep, fzf, requests) into suites: causal/decision sentences in commit messages = facts; a later commit touching the same files = the task (probe); fact is stale when lines its commit added no longer survive at T1. Suites are JSON (`bench/suites/`), tuning + held-out split. A tiny hand-built synthetic suite stays only as a smoke test.
- Time model: history <= T0 is fed to the tool, the repo is then checked out at T1 (staleness), probes come from commits after T1.
- Transcript-in sessions are rebuilt from commits: user task, Edit call with the real diff lines, assistant explanation (Claude Code JSONL).
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
Done (wip commits on feature/bench): crate skeleton, miner + 18 suites, tokens/matching/score, adapters none/dump/file/grep/recalla/recalla-backfill, MCP client, session writer, `run` table incl. extraction, safe path names.
- [x] A. Probe quality: a required fact must share a content word with the task or files (flask/requests have none; drop or remint); report probe counts per suite
- [x] B. Provenance scoring wired into `run` (mid budget, with files) via adapter `source`
- [x] C. Generic MCP adapter (command + tool names via flags) and a fake MCP server in tests
- [x] D. Report: raw per-probe JSON, where-each-tool-loses, tune vs holdout, weighted total (3 pillars, equal)
- [x] E. Hand spot-check of ~20 mined facts/probes; record findings in bench/README.md
- [x] F. bench/SCORING.md (lock definitions + weights), bench/README.md, DESIGN_DECISIONS (tempfile), `make bench`, CI smoke, CHANGELOG, state
- [ ] G. Real-data mode (`membench real`), local only
- Deferred: agent-run tier (billed; needs approval); claude-mem/supermemory (need LLM/network; separate tier, needs approval)

## Rules
All work on feature/bench into dev, never main. No tuning Recalla against the holdout split; only fix real defects (e.g. #48), each on its own branch.

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
