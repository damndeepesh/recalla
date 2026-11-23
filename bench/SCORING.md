# Scoring

Fixed before results are published. Every rule is mechanical and applies to every tool. Change a rule only with a new entry in `DESIGN_DECISIONS.md` and a re-run of all tools.

## What a tool is given

- **Feed**: each suite fact as one sentence plus the files it is about, `"<sentence> (files: a, b)"`, written through the tool's own write interface (MCP `remember`/`store`, a file, ...). Facts are only those from commits up to `t0`.
- **Task**: a later commit's subject (`probe.task`), asked with and without `(files: ...)` hints. Headline numbers use hints, as an agent would name the files it is about to edit.
- **Code**: with `--repos`, a real `git worktree` at `t0` that is moved to `t1` before the tasks are asked, so tools that watch code can notice changes.
- **Budget**: 500, 1500 and 4000 tokens. Tokens are `ceil(chars / 4)` plus one per item, the same counter for every tool. Tools without a budget parameter are cut by the harness in the order they return items.

## Per-probe metrics

| Metric | Definition | n/a when |
|---|---|---|
| recall | share of the probe's required (live, same-file) facts present in the reply | the probe has none |
| noise | 1 - share of returned tokens spent on facts about files other than the task's | nothing returned |
| staleness | per stale same-file fact: flagged stale 1, left out 0.5, served as current 0; mean | the probe has no stale fact, or nothing returned |
| provenance | share of returned required facts whose source the tool names (answer contains the first 7 characters of the fact's commit) | the tool has no provenance interface |
| efficiency | `max(0, 1 - reply_tokens / dump_tokens) x recall`: tokens saved, but only if the fact arrived | recall is n/a |

A fact is **present** when at least 70% of its content words occur in one returned item. An empty reply earns no credit for noise or staleness.

## Pillars and total

1. **Usefulness** = mean of the recall, noise, staleness and provenance means (those that apply).
2. **Token savings** = efficiency mean.
3. **Extraction** = share of suite facts found among the memories a tool extracted itself from session transcripts (`recalla-backfill`). Only tools that ingest transcripts have it. After a plain write the tool's store just echoes the feed, so that is not extraction.

`total` is the equal-weight mean of the pillars that apply; a pillar a tool cannot be scored on is n/a, not zero. `core` (usefulness and token savings) is the column to compare tools on when pillar counts differ.

## Splits

Probes and facts are split by commit-hash parity: `tune` was used while building the miner and tools, `holdout` never. Headline numbers are holdout only; the report prints both so a gap is visible. Tools are not tuned against holdout results; only defects found in tune data or by hand-checking get fixed.

## Known limits

- Facts are sentences from commit messages. About a fifth of the live/stale ones are weak (see `SPOTCHECK.md`); all tools are judged against the same labels.
- A probe keeps a fact only if it shares a word of 4+ characters with the task or its file paths. This is lexical, so keyword retrievers do well on recall by construction.
- Ground truth is code history, not agent transcripts. It measures what a memory should return, not whether an agent finishes tasks faster.
- The suites were mined by Recalla's author. Facts come from other people's repositories and the miner is deterministic, but read the numbers with that in mind and rerun them yourself.
