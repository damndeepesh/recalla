# Plan: Codex harness

## Goal
`recalla backfill` also imports Codex CLI sessions, feeding the same events → redact → extract → pack pipeline. One memory spans both agents (the cross-agent promise).

## Observed format (local logs, key names only)
- Location: `$CODEX_HOME` or `~/.codex`, under `sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`.
- Each line is `{type, timestamp, ordinal, payload}`.
  - `session_meta.payload`: `id`/`session_id`, `cwd`, `cli_version`, …
  - `turn_context.payload.cwd`: the cwd can change per turn.
  - `response_item` + `payload.type=message`: `role` user|assistant|developer; `content[]` of `input_text` / `output_text` / `input_image`.
  - `event_msg` + `payload.type=item_completed`: `item.type` is one of:
    - `CommandExecution` {command: argv list, cwd, exit_code, status, aggregated_output}
    - `FileChange` {changes: {path → change}}
    - UserMessage / AgentMessage, which duplicate response_item messages
  - `compacted.payload.message`: compaction summary.
  - Skipped: reasoning, token_usage_record, world_state, custom_tool_call*. Commands come from CommandExecution, which carries the exit code.
- User text carries injected context tags (`<environment_context>`, `<user_instructions>`, …); strip them like Claude's system-reminder.

## Design
- `harness::codex`: `discover(root)` (recursive under `sessions/`, symlinks skipped) and `parse_bytes`, with the same signature/contract as Claude.
  - Emits `Event` with `harness = "codex"`.
  - Mapping:
    - user `input_text` (stripped) → UserText
    - assistant `output_text` → AssistantText
    - developer role → skipped
    - CommandExecution → `ToolCall {name: "Bash", id: item.id}` with text `{"command": <shell string>}`, plus `ToolResult {tool_use_id: item.id, is_error: exit_code != 0}` with text `aggregated_output`
      - Shell string: argv `[sh|bash|zsh, -c|-lc, CMD]` → CMD; otherwise shell-join the argv.
    - FileChange → one `ToolCall {name: "Edit"}` per path with `{"file_path": path}`, which feeds the anchor window
    - compacted → Summary
  - `session_id` from session_meta; the cwd tracks session_meta and turn_context.
  - `message_uuid` = payload `id` or item `id`. Line and offset work as for Claude.
- Ingest: `Options` gets the harness roots. `backfill` discovers Claude AND Codex by default; `--harness claude|codex|all`.
  - Each source row records its harness, so identity, resume and archive are unchanged.
- Extraction is unchanged (harness-agnostic). Procedures/gotchas work via Bash + exit code.
- MCP registration for Codex is already printed by `hooks install`. Native Codex hooks are out of scope; capture for Codex happens on the next backfill.

## Steps
- [x] 1. `harness::codex` discover + parse + tag stripping; synthetic fixtures (`tests/fixtures/codex/…`) covering every mapped type, plus malformed lines, a partial tail and multibyte text
- [x] 2. Ingest/CLI: multi-harness backfill (`--harness`), with Codex root resolution (`CODEX_HOME`); tests: a mixed Claude + Codex root imports both, a rerun is a no-op, an appended Codex line resumes
- [x] 3. Eval: one Codex-format session in the extractor eval (convert one existing session's turns to Codex shape) with the same expected labels
- [x] 4. Real-data smoke on `~/.codex` (counts only) + docs

## Risks
- **Codex format drift** (it's young and changes often): lenient parsing, unknown types counted, never fatal.
- **Duplicate messages** (UserMessage/AgentMessage items repeat response_item messages): use only response_item messages.
