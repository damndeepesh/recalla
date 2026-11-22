# Recalla launch drafts (finalize numbers after anchors merge)

## X / Twitter thread

1/ Your coding agent already has months of your decisions sitting in its session logs. Most memory tools ignore all of it and start from zero.

Recalla imports it. One Rust binary, local, no LLM. 🧵

2/ `recalla backfill` turned 37 MB of my Claude Code history into memories in under a second:

- my rules ("never touch org X", "subscription only")
- decisions
- gotchas from errors
- the commands I actually run (`make check`, seen 15×)

3/ Every memory links back to the exact message it came from:
`recalla why <id>` shows the session, the line, and the original (redacted) text.

4/ Anchored to code: a memory about `Store::insert` is pinned to that function. Change the function, and the memory is flagged "needs re-verify". Edit a different function in the same file, and it isn't. No more confidently stale advice.

5/ Built not to eat your context window:

- Token-budgeted packs at session start
- 4 MCP tools in ~430 tokens (not 30)
- `recalla stats` shows exactly what was injected

6/ Private by default: secrets are redacted in 3 layers before anything is stored. Tested against 100+ real-world credential formats.
No daemon, no network, no LLM key.

7/ Pre-alpha, works today with Claude Code (Codex next):

cargo install --git https://github.com/damndeepesh/recalla
recalla backfill && recalla hooks install

Feedback very welcome 👇 <link>

## Show HN

**Title:** Show HN: Recalla – local memory for coding agents, built from your existing session history

**Body:**
Hi HN. Recalla is a single Rust binary that gives Claude Code (and soon Codex) a memory built from the history you already have.

What's different from the many memory tools out there:

- **Backfill.** It imports your existing session logs (resumable, idempotent) rather than starting from zero. 37 MB of my history imports in about 0.8 s.
- **Provenance.** Every memory links to the exact message it came from (`recalla why`).
- **Code anchors.** Memories that mention code are pinned to the symbol via tree-sitter. When that symbol changes, the memory is marked "needs re-verify" instead of being served as fact. Benign edits elsewhere in the file don't trigger it.
- **Cheap.** Token-budgeted context packs, a ledger of what was injected, and an MCP surface of 4 tools (~430 tokens).
- **Private.** No daemon, no network, no LLM. Secrets are redacted before storage, and the redactor is gated on a corpus of 108 real-world credential formats.

Extraction is rule-based. That's deliberate (deterministic, offline, free), but it misses things an LLM would catch. The eval and its limits are in the repo.

I'd love feedback, especially on extraction quality on your own logs. `recalla search` and `recalla stats` show what it found.

Repo: <link>

## Reddit (r/ClaudeAI)

**Title:** I built a local memory engine that learns from your existing Claude Code history (and flags memories when the code changes)

Body: a shorter version of Show HN, plus a screenshot of `recalla pack` and `recalla why`.

## Launch checklist

- [ ] Repo public again (the user decides when)
- [ ] `cargo install --git` works from a clean machine
- [ ] Demo GIF: backfill → hooks install → new session shows the pack → edit code → memory flagged
- [ ] README numbers refreshed
- [ ] Landing page (GitHub Pages), later per the user
