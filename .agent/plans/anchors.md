# Plan: code anchors + soft staleness

## Goal
Memories that talk about code get pinned to it. When the referenced symbol (or file) changes, the memory becomes `needs_reverify`: shown in its own pack section, ranked lower, never deleted. Benign edits elsewhere in the same file don't trigger it. `review confirm` / `recalla confirm` re-anchors. This is the differentiator: every other memory tool serves stale facts unmarked.

## Design
- **Anchor resolution** (`anchor` module) runs at extraction time and on `confirm`:
  1. Paths. Use the memory's `anchors` (path tokens) plus the file_path of the triggering tool call, already captured. Resolve each relative to the project root (the memory's Project scope key), or keep it absolute. **Must canonicalize under the project root**; anything outside is dropped (security note from the ingest review). Skip missing files.
  2. Symbols. Collect candidate identifiers from the memory text: backticked spans, `CamelCase`, `snake_case` (≥4 chars) and `a::b`/`a.b` paths. Keep those that a tree-sitter query finds **defined** in the anchored file: fn/method/struct/enum/trait/impl/class/interface/type/const. This gives `Anchor { path, symbol, lang, content_hash, git_commit }`.
  3. Hash. blake3 of the symbol's definition node text with whitespace runs collapsed, so a reformat ≠ change. Comments inside count as content. File-only anchors (no symbol) store the hash of the whole file but are checked only for existence/rename (see staleness).
- **Languages:** Rust, Python, Go, TypeScript/TSX, JavaScript via tree-sitter grammars. Each is behind a cargo feature (`lang-rust` …), all on by default. Unknown extensions get a path-only anchor.
- **Staleness check** (`anchor::check(store, project_root)`):
  - symbol anchor: file missing → stale ("file removed"); symbol not found → stale ("symbol removed"); hash differs → stale ("symbol changed").
  - path-only anchor: stale only if the file is gone. Whole-file edits are too noisy.
  - A memory with ANY stale anchor → `set_status(NeedsReverify)`. Record the reason in a new `anchors.stale_reason` column (migration v5).
  - Cached by (path, mtime, size) per run. Only memories of the current project are checked.
  - When it runs:
    - `recalla check [--project P]` explicitly
    - automatically inside `pack` / `hook session-start` for the candidate set, bounded to N files and a ≤50 ms budget, skipping the rest
    - after `capture` for files touched in the new events
- **Confirm:** `recalla confirm <id>` and MCP `review confirm` re-resolve the anchors (new hashes), clear the stale reason and set the memory back to active.
- **Pack:** the "Needs re-verify ⚠" section shows the reason, e.g. `(symbol changed: Store::insert)`, so the agent knows what to re-check.
- **Git commit:** record `HEAD` if the project is a git repo. Read it directly from `.git/HEAD`/refs, no git2 dep. This is informational only.

## Steps
- [x] 1. Deps tree-sitter + 5 grammars behind features; ADR D-019 (tree-sitter vs regex symbol maps: accuracy, vs binary size — measure before/after release size and report it)
- [x] 2. `anchor::lang`: extension → grammar + definition query; `definitions(src) -> Vec<(name, byte_range)>`; unit tests per language
- [x] 3. `anchor::resolve(memory_text, paths, project_root) -> Vec<Anchor>` with root-containment checks, candidate identifiers, normalized hashing; tests incl. path escape refusal
- [x] 4. Migration v5 (`anchors.stale_reason`) + store APIs (anchors_for_project, set_anchor, mark_stale, clear_stale)
- [x] 5. Wire into extraction (anchor after insert), `check` CLI, pack auto-check (time-budgeted), capture post-check, `confirm` CLI + MCP review confirm
- [x] 6. Tests:
  - change the anchored fn body → stale
  - change another fn in the same file → not stale
  - reformat the anchored fn → not stale
  - delete the file → stale
  - rename the symbol → stale
  - confirm → active again
  - path outside root never anchored
  - pack shows the reason
  - a time budget is respected
- [x] 7. Real-data smoke on this repo (memories about `Store::insert`, `redact`, …) + docs

## Risks
- **Binary size from grammars:** measure it, and keep the feature flags.
- **False "changed" on formatting:** whitespace normalization; a reformat test.
- **Hook latency:** the auto-check is bounded and cached; the full check is explicit.
- **Path traversal:** canonicalize + prefix check, tested.
