# Plan: MCP server + Claude Code hooks

## Goal

Make Recalla usable by agents with zero manual steps:

- `recalla hooks install` injects a context pack at every Claude Code SessionStart and captures new turns on Stop/PreCompact.
- `recalla mcp` exposes 4 tools (`tools/list` ≤1k tokens) to Claude Code, Codex and any MCP client.

## MCP server (`recalla mcp`, stdio)

- **Hand-rolled JSON-RPC over stdio:** no tokio and no rmcp (ADR D-017). The reasons are binary size, instant startup and only 4 tools. Use a sync loop over `BufRead::lines()`.
  - Read one JSON message per line and write one per line (no embedded newlines).
  - stdout carries ONLY MCP messages; logs go to stderr.
- **Dual era:**
  - 2026-07-28 (stateless): every request carries `params._meta["io.modelcontextprotocol/protocolVersion"]`.
    - `server/discover` returns `{resultType:"complete", supportedVersions:["2026-07-28","2025-11-25"], capabilities:{tools:{}}, _meta:{"io.modelcontextprotocol/serverInfo":{name:"recalla",version}}, instructions, ttlMs:3600000, cacheScope:"public"}`.
    - An unknown version returns error -32022 `{supported, requested}`.
  - 2025-11-25 legacy: `initialize` returns `{protocolVersion (echo if supported, else "2025-11-25"), capabilities:{tools:{}}, serverInfo:{name,version}, instructions}`. Accept `notifications/initialized` (no response).
  - Notifications (no `id`) never get a response.
- **tools/list:** `{resultType:"complete", tools:[...]}`. Include `resultType` only for 2026 requests, since older clients may reject unknown fields. Legacy clients get `{tools}`.
- **tools/call:** `{content:[{type:"text",text}], isError:false}` (+`resultType` for 2026). An unknown tool is JSON-RPC error -32602; a tool failure is `isError:true` with an actionable message.
- **Tools (descriptions short and imperative, schemas minimal; a test measures `est_tokens(tools/list JSON)` ≤ 1000):**
  1. `recall {query?, budget?=1200, project?}` returns a pack (markdown). It records an injection with via='mcp'. The default project comes from `CLAUDE_PROJECT_DIR` env, else cwd.
  2. `remember {text, kind?=fact, scope?="project"|"global"}`:
     - inserts with `Origin::Remember`
     - the text goes through `redact` FIRST
     - rejects empty text or text over 1000 chars
     - returns the uid
  3. `review {id, action: "confirm"|"forget"}`: confirm is `set_status(Active)` + bump confidence to max(0.9, c); forget is `archive` (reversible). The id is a uid prefix ≥4 chars; an ambiguous prefix is an error that lists candidates.
  4. `why {id}`: memory + provenance + source event text. The same output as `recalla why`; reuse that formatter.
- **Robustness:** a malformed JSON line gets -32700, and an unknown method -32601. The server never panics on input, and the DB is opened lazily per call. A missing DB means `recall` returns an empty pack and `remember` creates the DB.

## Hooks (`recalla hook <event>`, `recalla hooks install|uninstall|status`)

- `recalla hook session-start`:
  - reads stdin JSON `{session_id, transcript_path, cwd, source, ...}`
  - builds a pack for `project_key(cwd)` with a default budget of 1500 (`RECALLA_BUDGET` env override)
  - prints `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"<pack>"}}`
  - records an injection with via='hook'
  - with an empty pack: no output, exit 0
- `recalla hook capture` (for Stop and PreCompact):
  - reads stdin, then ingests ONLY `transcript_path` (incrementally, same pipeline) and runs extraction for the new events
  - must be fast and silent: no stdout, and always exit 0, even on error. Errors go to `<data>/log/hook.log`, never to stdout or stderr shown to the model.
  - ignores `stop_hook_active` loops, since it never blocks
- Any hook error must never block the session: always exit 0. Hard deadline ~5 s for session-start: if the DB is locked, give up and return empty.
- **`hooks install`:** edit `~/.claude/settings.json`, or `--settings PATH` for tests.
  - Parse the JSON. Create a timestamped backup next to it, then add entries under `hooks.SessionStart`, `hooks.Stop` and `hooks.PreCompact`: `{matcher? none, hooks:[{type:"command", command:"<abs path to recalla> hook session-start", timeout: 10}]}`.
  - Idempotent: detect existing entries by a command containing `recalla hook`, and update them in place.
  - Never touch other hooks or keys. Preserve unknown fields. Write atomically (tmp + rename), keeping the file's permissions.
  - Also print the MCP registration commands. Don't run them; let the user opt in:
    - `claude mcp add --scope user recalla -- <abs path> mcp`
    - Codex: `[mcp_servers.recalla] command = "<abs>" args = ["mcp"]` in `~/.codex/config.toml`
- **`hooks uninstall`:** remove only entries whose command contains `recalla hook`; drop event arrays that end up empty; back up first.
- **`hooks status`:** shows installed or not, the settings path, and the last hook-log lines.

## Steps

- [x] 1. `mcp` module: framing loop, dispatch, dual-era handshake, errors; unit tests with scripted stdin/stdout (in-memory reader and writer)
- [x] 2. 4 tools + the shared `why` formatter; `remember` redacts; tools/list token test
- [x] 3. `hook` module: session-start + capture (ingest a single file, then extract); always exit 0; hook log
- [x] 4. `hooks install/uninstall/status` with JSON-preserving edits; tests over temp settings files covering: empty, existing other hooks, already installed, malformed JSON (refuse, don't overwrite), uninstall restores
- [x] 5. Integration:
  - spawn `target/debug/recalla mcp` as a child process, script a legacy `initialize` session and a 2026 `server/discover` session
  - call each tool against an eval-fixture DB
  - hook session-start JSON output
  - capture ingests only the given transcript
- [x] 6. ADR D-017 (hand-rolled MCP) + D-018 (hooks never block; settings edit policy); docs; README usage

## Risks

- **Spec drift:** dual-era support plus tests for both handshakes.
- **Corrupting the user's settings.json:** backup first, refuse on malformed input, atomic write, round-trip tests that preserve unknown keys.
- **Hook latency:** session-start runs on every session. Target <100 ms, measured in the perf item.
- **`remember` as an injection channel:** a prompt-injected agent could write junk memories. Mitigation: redact, length cap, `Origin::Remember` provenance shown in `why`, and `review forget` to undo.
