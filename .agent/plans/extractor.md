# Plan: rule-based extractor

## Goal

Turn stored (already redacted) events into typed, atomic memories with confidence, scope and message-level provenance. Deterministic, offline, no LLM (D-004). Measure it against a labeled eval set: precision ≥0.8 is the SPEC acceptance bar, and recall is reported too. Re-runs are incremental and idempotent.

## Design

- **Input:** the `events` table, processed in order per session, starting from the `extract_state` checkpoint (last event id processed). It never touches raw logs or the archive.
- **Extractors** (each a small pure fn `&EventCtx -> Vec<Candidate>`):

  | Kind       | Source                | Signal                                                                                                                                                                                                                                                                                                      |
  | ---------- | --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | preference | user text             | Directive markers: `always`, `never`, `don't`/`do not`, `make sure`, `prefer`, `instead of`, `from now on`, `remember (that)`, `stop doing`. Sentence-level, imperative.                                                                                                                                    |
  | decision   | assistant + user text | `decided`, `we'll use`, `going with`, `chose X over Y`, `switch(ed) to`, `decision:`, `instead of X we`. Also `## D-NNN` entries in DESIGN_DECISIONS.md writes (tool_call Write/Edit to that file), at high confidence.                                                                                     |
  | gotcha     | error→fix sequences   | A `tool_result is_error` followed within N events by assistant text with `root cause`, `the issue was`, `turns out`, `fixed by`, `the fix`, `because`. The memory is the explanatory sentence.                                                                                                              |
  | procedure  | tool calls            | Successful shell commands whose result is not an error, normalized (strip paths/args after flags) and recurring across ≥2 sessions, or matching the build/test/run vocabulary (`make`, `cargo`, `npm run`, `pytest`, `go test`…). Text: `Run \`<cmd>\` to <verb>`, with the verb inferred from the command. |
  | task       | TodoWrite + TaskCreate/TaskUpdate calls | Each todo gets a stable key (content hash). The latest status wins: an older task memory is superseded when its status changes. Completed tasks are archived.                                                                                                                                               |
  | fact       | user text             | Narrow: `X is (located                                                                                                                                                                                                                                                                                      | in  | at  | on) Y`, `we use X for Y`, `the X is Y` where X is a code-ish noun. Conservative confidence. |

- **Sentence handling:**
  - Strip fenced code blocks and inline JSON.
  - Split on sentence boundaries.
  - Keep sentences of 15–280 chars.
  - Drop questions, and drop text dominated by `[REDACTED:` markers.
- **Scope:**
  - `Project(key)`, where key is the event's cwd normalized to its git toplevel. The git root is resolved by walking up for `.git` on disk; if the path no longer exists, cwd is used.
  - Global for preferences without project-specific nouns: the sentence names no file, path or crate.
  - Module scope is left to the anchors item.
- **Confidence:** base per rule (decision-ADR 0.95, explicit user directive 0.85, assistant decision 0.7, gotcha 0.65, procedure 0.6 + 0.1 per extra session, max 0.9, fact 0.5). Importance comes from the kind: decision/preference 2, gotcha/procedure 1, task 1, fact 0.
- **Provenance:** `Origin::Backfill`, harness, session_id, message_uuid, source_path, line. Dedup merges via the uid (existing store behaviour), so repeated memories bump `seen_count`.
- **Anchors (lightweight now):** file paths named in the same event, or in the file_path of the triggering tool call, become `Anchor { path }`. Symbol and hash come in the anchors item.
- **Store:** migration adds `extract_state(id=1, last_event_id, extractor_version)`.
  - When `extractor_version` bumps, `recalla extract --rebuild` archives every memory whose only provenance is `backfill`, then re-extracts. Archived, never deleted.
- **CLI:**
  - `recalla extract [--rebuild] [--json]`
  - `recalla backfill` runs ingest, then extract.
  - `recalla search <query> [--kind K] [--project P] [--limit N]` is a dev/inspection command over `Store::search`. It is not the MCP surface.
- **Eval:** `tests/fixtures/extract/*.jsonl` holds synthetic sessions, each with a sidecar `*.expected.json` listing (kind, text-substring) pairs. `tests/extract_eval.rs` computes per-kind precision and recall and asserts precision ≥0.8 overall and per kind with ≥5 labels. The numbers are printed for the PR.

## Steps

- [x] 1. Migration (v3, or amend if unshipped): `extract_state`; store APIs `events_after(id, limit)`, `extract_checkpoint`, `archive_backfill_memories`
- [x] 2. `extract` module scaffold: `EventCtx` (event + session window), `Candidate`, sentence splitter + code stripping, scope resolver (pure fn + fs wrapper)
- [x] 3. preference + decision extractors (incl. ADR parsing) + unit tests
- [x] 4. gotcha (error→fix window) + procedure (command normalization, cross-session recurrence) + unit tests
- [x] 5. fact extractor + task extractor (TodoWrite and TaskCreate/TaskUpdate; lowest priority: real logs show 0 todo calls, Bash dominates at 1055 calls and there are 41 error results)
- [x] 6. Pipeline: batched, incremental, checkpointed; `Report` (memories new/duplicate per kind, events processed)
- [x] 7. CLI: `extract`, `search`, and backfill→extract
- [x] 8. Eval set (≥40 labeled memories across kinds, plus negatives) + eval test; tune to precision ≥0.8
- [x] 9. Real-data smoke: run on the ingested DB and report counts per kind. Inspect ~20 memories of this dev session (my own conversation) to sanity-check quality.
- [x] 10. Docs

## Risks

- **Junk memories** (mem0's failure mode). Mitigation: precision-first rules, an eval gate, and length/question filters. Low confidence ranks lower in packs.
- **Over-fitting to English phrasing.** Acceptable for MVP; documented.
- **Scope resolution touching disk.** It only reads `.git` existence under the cwd path and never follows paths from content beyond cwd. A path that no longer exists falls back to cwd.
- **Task churn.** Supersede chains stay bounded because completed tasks are archived.
