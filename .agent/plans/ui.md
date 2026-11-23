# Plan: `recalla ui` — local dashboard + connector status (v0.2.0)

## Goal

Open one local app and see every session and every memory, see exactly what each agent session was given (and how many tokens that saved versus dumping everything), and link agents (Claude Code, Codex) with clear connection status. Everything stays local; the same single binary serves it.

## Decision: local web UI in the binary, not Tauri/Electron (for now)

- No install, signing, notarization or extra toolchain; works on every platform we ship; keeps "one binary, no daemon".
- It only runs while the user has it open (`recalla ui`, Ctrl-C to stop).
- The UI is plain HTML/CSS/vanilla ES modules (no node, no build step) embedded with `include_str!`. A Tauri shell can wrap the same files later.
- The "connector" already exists: hooks (capture + inject) and the MCP server. The app makes it visible, verifiable and one-click to set up. It is not a second protocol.

## Security (new attack surface: a localhost HTTP server; reviewed by Opus before release)

- Bind 127.0.0.1 only; port 0 (random) by default, `--port` to fix it.
- A random 256-bit token per run. First load: `http://127.0.0.1:<port>/?t=<token>` sets a `SameSite=Strict; HttpOnly` cookie and redirects to a clean URL. Every `/api` call requires the cookie or `Authorization: Bearer`.
- `Host` header must be `127.0.0.1:<port>` or `localhost:<port>` (DNS-rebinding defense). No CORS headers at all.
- Mutating calls (POST) additionally require the `Origin` header to match, and a custom `X-Recalla-Csrf` header equal to a per-session value (double defense).
- Strict CSP: `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'`; plus `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `Cache-Control: no-store` on API.
- The UI renders untrusted text (it comes from agent logs): ONLY `textContent`/`createElement`, never `innerHTML`; server also strips control and invisible characters (reuse `pack::one_line`-style sanitizer) on every string it returns.
- Data comes from the already-redacted DB; the raw archive is never served. File paths in responses are shown as basenames plus project keys.
- Read-only DB connection for GETs; writes only through the same `ops::` functions the CLI uses (confirm/forget/restore/sync).
- Request limits: max header/body size, per-connection timeouts, bounded page sizes.
- No telemetry, no third-party requests, no `eval`.

## Data it shows

1. **Overview:** memories by kind and status, sessions, events, DB/archive size, stale count; injection stats; connector health.
2. **Sessions:** list (agent, project, first/last time, events, memories extracted) → detail: timeline of events (user, assistant, tool calls and results, truncated), the memories that came from it, and the context pack the agent received at its start.
3. **Memories:** filter by kind/status/project/agent, FTS search, sort; detail with full text, provenance links to sessions, anchors with staleness reasons; actions: confirm, forget (archive), restore. This also gives humans the missing "forget" (issue #27).
4. **What Claude got (injections):** every injection with project, via (hook/mcp/cli), session, budget, est. tokens, and the exact pack text. Shows "this pack: 412 tokens vs all N memories for this project: ~M tokens (−94%)". This is the "optimised, only what it needs" proof.
5. **Connect:** hooks status per event, MCP registration commands (Claude Code, Codex) with copy buttons, data/mirror paths, last capture time, hook log tail, "Test recall" button (runs a real pack for a chosen project), "Import history" button (runs backfill + extract in-process; shows the report).

## Backend

- Minimal sync HTTP/1.1 server on `std::net` with the `httparse` crate for request parsing (DECIDED: `tiny_http` was last released in 2022 and pulls TLS crates; `httparse` is maintained, zero-dependency and the parser inside hyper). NO tokio/axum (binary size, startup). Thread per connection with a hard cap, `Connection: close`, no keep-alive, no chunked bodies. ADR D-022.
- JSON API (`serde_json`, already a dep): `GET /api/overview`, `/api/sessions`, `/api/sessions/{id}`, `/api/sessions/{id}/events`, `/api/memories`, `/api/memories/{uid}`, `/api/injections`, `/api/injections/{id}`, `/api/connect`; `POST /api/memories/{uid}/{confirm|forget|restore}`, `/api/sync`, `/api/test-recall`. Keyset pagination, page ≤ 200.
- Schema v8: `injections` gains `session_id TEXT` and `pack_text TEXT` (nullable; older rows stay valid). Hook `session-start` (has `session_id` in its stdin), MCP recall and CLI pack record them. Add an index on `injections(session_id)`.
- All SQL lives in `store` (new `store/ui.rs`); handlers live in `ui/api.rs`; server in `ui/server.rs`; assets in `ui/assets/`.
- `recalla ui [--port N] [--no-open] [--data-dir D]`: prints the URL, opens the default browser (`open` on macOS, `xdg-open` on Linux, via `std::process::Command`, no dep), runs until Ctrl-C.

## Frontend

- Single-page app, hash routing, vanilla ES modules, CSS variables, light/dark, responsive, keyboard accessible, WCAG AA. No framework, no build step. Visual language shared with the landing page.
- Lists are virtualised-by-pagination; the detail drawer lazy-loads.
- Live-ish: poll `/api/overview` every 3 s while the tab is visible (no websockets).

## Steps

- [ ] 1. HTTP server core: bind, token+cookie, Host/Origin/CSRF checks, CSP headers, static asset serving, request limits, graceful Ctrl-C; unit and integration tests, including attack tests (wrong Host, missing token, cross-origin POST, oversized request, path traversal on static paths)
- [ ] 2. Schema v8 + store query APIs (`store/ui.rs`) + recording `session_id`/`pack_text` at hook/MCP/CLI injection points; tests; perf on the 100k-memory fixture (list/search p95 < 100 ms)
- [ ] 3. API handlers + JSON contract (documented in ARCHITECTURE) + sanitization + mutation handlers via `ops::`; API tests with a populated DB
- [ ] 4. Frontend (designer agent, Sonnet): all five views against the real API; verify in a real browser (screenshots at 3 widths, light and dark, keyboard path, Lighthouse a11y ≥ 95, console clean, zero third-party requests)
- [ ] 5. CLI `recalla ui`, browser open, e2e checks added to `scripts/e2e.sh` (start server on port 0, curl with/without token, rebinding Host, CSRF, then a full session→memory→UI path), docs (README, ARCHITECTURE, ADR), landing page "available" update
- [ ] 6. Opus security review (network surface) → fixes → PR → v0.2.0 (version bump, CHANGELOG, publish, tag; the release workflow already builds binaries)

## Risks

- A localhost server is the first network-facing code. Mitigated by the model above and a dedicated review.
- Scope: keep v1 to the five views; no editing of memory text, no settings.json editing from the browser (copy-paste commands plus the existing CLI do that safely).
- Frontend without a framework can sprawl: small modules per view, one tiny DOM helper, no state library.
