# membench

A benchmark for coding-agent memory tools. It asks one question: given the history of a project and a new task, does the tool hand the agent the right facts, within a token budget, without noise, and without serving things the code has since contradicted?

Ground truth is mined from the git history of six public repositories (ripgrep, fzf, requests, tokio, flask, caddy; three epochs each, 18 suites, 240 probes). No LLM, no network at run time, no API keys. The same fixed rules score every tool; see [SCORING.md](SCORING.md).

## Run it

```sh
make bench                       # from the repo root: baselines + the bundled fake MCP server
make bench REPOS=~/clones        # with real worktrees (clones made by suites/build.sh)
```

By hand, with any MCP memory server that has a store tool and a recall tool:

```sh
cargo run --release -- run --suite suites/*-e?.json \
  --adapters none,grep,mcp \
  --mcp-command "my-memory-server --stdio" --mcp-name my-memory \
  --mcp-store-tool remember --mcp-text-arg text \
  --mcp-recall-tool recall --mcp-query-arg query \
  --split holdout --report report.md --out raw.json
```

`--report` writes totals, tune vs holdout and where each tool loses. `--out` writes every probe's raw score as JSON. Adapters: `none` (no memory), `dump` (everything), `file` (a CLAUDE.md-style file, newest first), `grep` (word overlap), `mcp` (any MCP server), `recalla` and `recalla-backfill` (need `--recalla PATH`).

## First results

Holdout probes, 1500-token budget, file hints, real worktrees. Recalla at `dev` after #49.

| tool | usefulness | token savings | core | extraction |
|---|---|---|---|---|
| none | 0.00 | 0.00 | 0.00 | n/a |
| dump | 0.40 | 0.00 | 0.20 | n/a |
| file | 0.32 | 0.15 | 0.24 | n/a |
| grep | 0.44 | 0.38 | 0.41 | n/a |
| fake-mcp (toy word-overlap server) | 0.48 | 0.59 | 0.53 | n/a |
| recalla (`remember`) | 0.55 | 0.69 | 0.62 | n/a |
| recalla-backfill (extracts from transcripts) | 0.26 | 0.04 | 0.15 | 0.05 |

Read these carefully:

- grep wins raw recall (0.99 vs Recalla 0.94) but spends about 1180 tokens against Recalla's 490, with more noise. The lexical probe filter favours keyword retrieval.
- Recalla's extraction is poor on this feed: 5% of suite facts, from plain explanatory sentences. The extractor targets error-then-fix patterns and preferences; widening it is tracked as a follow-up.
- Provenance is measured only where a tool keeps a source (transcript ingestion). Recalla via `remember` has none, so it is n/a, not zero.
- Not run: claude-mem, supermemory and other tools that need an LLM or network. They belong in a separate, explicitly approved tier.
- The suites were mined by Recalla's author. Rerun everything yourself.

## Layout

- `suites/*.json` are the data; `suites/build.sh` regenerates them from `suites/sources.txt` (clones once, then offline).
- `src/` has the miner, adapters, scorers and report. `tests/` runs the generic MCP adapter against a bundled fake server.
- Licence: Apache-2.0. Suites contain short sentences quoted from commit messages of the repositories listed in `suites/sources.txt` (each suite records its licence).
