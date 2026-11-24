#!/usr/bin/env bash
# Live round trip with the real agent CLIs (not run in CI: needs `claude`, optionally `codex`,
# and Recalla hooks installed). Uses a scratch data dir and project; your real memories are untouched.
#   session 1 states a decision and a preference -> capture hook -> extractor
#   session 2 (fresh) must get them back via the SessionStart pack
#   codex (if installed) must get the preference over MCP
set -u
BIN=${RECALLA_BIN:-$(command -v recalla || true)}
[ -x "${BIN:-}" ] || { echo "recalla not found (set RECALLA_BIN)"; exit 2; }
command -v claude >/dev/null || { echo "claude CLI not found"; exit 2; }
"$BIN" hooks status 2>&1 | grep -qi 'not installed\|missing' && { echo "hooks not installed: run '$BIN hooks install'"; exit 2; }

W=$(mktemp -d); trap 'rm -rf "$W"' EXIT
export RECALLA_HOME=$W/home
mkdir -p "$W/proj" && cd "$W/proj" && git init -q
PROJ=$(pwd -P) # canonical: macOS /var is /private/var, and projects are keyed by the real path
pass=0 fail=0
ok() { if [ "$2" = 0 ]; then pass=$((pass+1)); echo "ok    $1"; else fail=$((fail+1)); echo "FAIL  $1"; fi; }

echo "== session 1: state a preference and a decision"
claude -p --model haiku --permission-mode acceptEdits "I prefer tabs over spaces in this repo, don't switch it. We decided to use SQLite WAL mode with busy_timeout 5000 because concurrent hook processes otherwise fail with 'database is locked'. Write that to notes.txt, then reply done." >/dev/null 2>&1
"$BIN" stats 2>&1 | grep -q 'preference' ; ok "capture + extract made a preference" $?
"$BIN" search WAL 2>&1 | grep -q 'WAL' ; ok "capture + extract made the WAL decision" $?

echo "== session 2 (fresh): pack must come back"
A=$(claude -p --model haiku "Without using any tools: which indentation style does this project use, and which SQLite journal mode? Quote any Recalla memory." 2>&1)
echo "$A" | grep -qi 'tabs' ; ok "fresh Claude session recalls the preference" $?
echo "$A" | grep -qi 'WAL' ; ok "fresh Claude session recalls the decision" $?
"$BIN" stats 2>&1 | grep -q 'injections  [1-9]' ; ok "injection logged in the ledger" $?
if command -v sqlite3 >/dev/null; then
  [ "$(sqlite3 "$RECALLA_HOME/recalla.db" "select count(*) from events where kind='assistant_text'")" -gt 0 ] ; ok "assistant text was ingested (tail catch-up)" $?
fi

if command -v codex >/dev/null; then
  echo "== codex over MCP (read-only tools auto-approved per tool)"
  C=mcp_servers.recalla
  O=$(timeout 180 codex exec --skip-git-repo-check -c "$C.command=\"$BIN\"" -c "$C.args=[\"mcp\",\"--data-dir\",\"$RECALLA_HOME\",\"--project\",\"$PROJ\"]" -c "$C.tools.recall.approval_mode=\"approve\"" "Call the recalla recall tool, then say in one line which indentation style the project uses." 2>&1)
  echo "$O" | grep -qi 'tabs' ; ok "codex recalls the preference over MCP" $?
else
  echo "codex not installed: skipped"
fi
echo "RESULT: $pass passed, $fail failed"; [ "$fail" = 0 ]
