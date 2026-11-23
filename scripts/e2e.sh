#!/bin/bash
# End-to-end functional checklist for recalla. Usage: scripts/e2e.sh <path-to-recalla-binary>  (or: make e2e)
# Everything runs in a scratch dir; the real ~/.claude and data dirs are never written.
B=$(cd "$(dirname "$1")" && pwd -P)/$(basename "$1")   # absolute: checks cd into the fixture project
S=${RECALLA_E2E_DIR:-${TMPDIR:-/tmp}/recalla-e2e}
mkdir -p "$S" && S=$(cd -P "$S" && pwd -P)   # real path: agents record getcwd(), not symlinked spellings (/var -> /private/var on macOS)
W=$S/e2erun
rm -rf "$W"
mkdir -p "$W/proj/src" "$W/claude/projects/-tmp-e2e" "$W/data"
PROJ=$W/proj
D=$W/data
P=0
F=0
pass() {
	echo "PASS  $1"
	P=$((P + 1))
}
fail() {
	echo "FAIL  $1  :: $2"
	F=$((F + 1))
}
ok() { if [ "$2" = "0" ]; then pass "$1"; else fail "$1" "exit=$2 ${3:-}"; fi; }

# ---- fixtures (secrets are built at runtime, never stored in source) ----
GH="ghp_$(python3 -c "import random,string;print(''.join(random.choices(string.ascii_letters+string.digits,k=36)))")"
PW="$(python3 -c "import random,string;print(''.join(random.choices(string.ascii_letters+string.digits,k=18)))")"
cat >"$PROJ/src/store.rs" <<'RS'
pub struct Store;
impl Store {
    pub fn insert(&self) -> u32 {
        1
    }
    pub fn other(&self) -> u32 {
        2
    }
}
RS
python3 - "$PROJ" "$W/claude/projects/-tmp-e2e/sess1.jsonl" "$GH" "$PW" <<'PY'
import json,sys
proj,out,gh,pw=sys.argv[1:5]
n=[0]
def rec(role,content,typ=None):
    n[0]+=1
    return {"type":typ or role,"uuid":f"u{n[0]}","sessionId":"sess1","timestamp":f"2026-11-01T10:00:{n[0]:02d}.000Z","cwd":proj,"message":{"role":role,"content":content}}
L=[
 rec("user","Never commit with failing tests."),
 rec("user",f"For the deploy use token {gh} and password={pw} please."),
 rec("assistant",[{"type":"text","text":"We decided that `Store::insert` in src/store.rs must run inside a transaction."}]),
 rec("assistant",[{"type":"tool_use","id":"toolu_e2e1","name":"Bash","input":{"command":f"cd {proj} && make check 2>&1 | tail -5"}}]),
 rec("user",[{"type":"tool_result","tool_use_id":"toolu_e2e1","content":"ok","is_error":False}]),
 rec("assistant",[{"type":"tool_use","id":"toolu_e2e2","name":"Bash","input":{"command":"cargo build"}}]),
 rec("user",[{"type":"tool_result","tool_use_id":"toolu_e2e2","content":"error[E0432]: unresolved import","is_error":True}]),
 rec("assistant",[{"type":"text","text":"The root cause is that the module was renamed last week, so the import is stale."}]),
]
open(out,"w").write("\n".join(json.dumps(x) for x in L)+"\n")
PY
CODEX=$(cd "$(dirname "$0")/.." && pwd)/tests/fixtures/codex

echo "== 1. basics"
$B --version >/dev/null
ok "--version" $?
$B --help | grep -q backfill
ok "--help lists commands" $?
$B pack --data-dir "$W/none" >/dev/null 2>&1
ok "pack with no DB: silent, exit 0" $?
[ -z "$($B pack --data-dir "$W/none" 2>/dev/null)" ]
ok "pack with no DB: empty stdout" $?
$B search x --data-dir "$W/none" >/dev/null 2>&1
[ $? = 2 ]
ok "search with no DB exits 2" $?

echo "== 2. backfill (claude + codex), idempotence, dry-run"
$B backfill --data-dir "$W/dry" --root "$W/claude" --codex-root "$CODEX" --dry-run >/dev/null 2>&1
ok "dry-run" $?
[ ! -e "$W/dry" ]
ok "dry-run writes nothing" $?
OUT=$($B backfill --data-dir "$D" --root "$W/claude" --codex-root "$CODEX" 2>&1)
ok "backfill both harnesses" $?
echo "$OUT" | grep -q 'claude-code 1, codex 1'
ok "report shows claude-code 1, codex 1" $?
$B backfill --data-dir "$D" --root "$W/claude" --codex-root "$CODEX" 2>&1 | grep -q '2 unchanged'
ok "rerun is a no-op (2 unchanged)" $?
echo '{"type":"user","uuid":"u99","sessionId":"sess1","timestamp":"2026-11-01T11:00:00.000Z","cwd":"'"$PROJ"'","message":{"role":"user","content":"Always run cargo fmt first."}}' >>"$W/claude/projects/-tmp-e2e/sess1.jsonl"
$B backfill --data-dir "$D" --root "$W/claude" --codex-root "$CODEX" 2>&1 | grep -q '1 resumed'
ok "appended line is resumed" $?

echo "== 3. secrets never reach the DB"
dump=$(sqlite3 "$D/recalla.db" "select text from events; select coalesce(cwd,'') from events; select text from memories;")
echo "$dump" | grep -qF "$GH"
[ $? -ne 0 ]
ok "GitHub token absent from DB text" $?
echo "$dump" | grep -qF "$PW"
[ $? -ne 0 ]
ok "password absent from DB text" $?
echo "$dump" | grep -q 'REDACTED'
ok "redaction markers present" $?

echo "== 4. extraction + search + why"
sqlite3 "$D/recalla.db" "select kind from memories" | sort | uniq -c | tr '\n' ' '
echo
$B search "failing tests" --data-dir "$D" | grep -qi 'never commit'
ok "preference extracted and searchable" $?
sqlite3 "$D/recalla.db" "select count(*) from memories where kind='procedure' and text like '%make check%'" | grep -q '^1$'
ok "compound command normalized to 'make check'" $?
sqlite3 "$D/recalla.db" "select count(*) from memories where kind='gotcha' and text like '%root cause%'" | grep -q '^1$'
ok "gotcha extracted from error then cause" $?
UID8=$(sqlite3 "$D/recalla.db" "select substr(uid,1,8) from memories where kind='decision' limit 1")
$B why "$UID8" --data-dir "$D" | grep -q 'provenance'
ok "why shows provenance" $?

echo "== 5. pack: budget, project scope, ledger"
PK=$(cd "$PROJ" && $B pack --data-dir "$D" --budget 600)
echo "$PK" | grep -q 'Recalla memory'
ok "pack has a header" $?
echo "$PK" | grep -qi 'never commit'
ok "pack includes the project preference" $?
TOK=$(echo "$PK" | python3 -c "import sys;t=sys.stdin.read();print(-(-len(t.encode())//3)+2*(t.count('\n')+1))")
[ "$TOK" -le 600 ]
ok "pack within budget (est $TOK <= 600)" $?
[ -z "$(cd /tmp && $B pack --data-dir "$D" --budget 600)" ]
ok "an unrelated project gets no foreign memories" $?
$B stats --data-dir "$D" | grep -q 'injections'
ok "stats shows the ledger" $?

echo "== 6. code anchors: staleness end to end"
sqlite3 "$D/recalla.db" "select count(*) from anchors where symbol like '%insert%'" | grep -qv '^0$'
ok "decision anchored to Store::insert" $?
$B check --project "$PROJ" --data-dir "$D" 2>&1 | grep -q '0 newly stale'
ok "check: clean at first" $?
python3 - "$PROJ/src/store.rs" <<'PY'
import sys;p=sys.argv[1];s=open(p).read().replace("pub fn other(&self) -> u32 {\n        2","pub fn other(&self) -> u32 {\n        22");open(p,"w").write(s)
PY
$B check --project "$PROJ" --data-dir "$D" 2>&1 | grep -q '0 newly stale'
ok "editing ANOTHER fn does not flag the memory" $?
python3 - "$PROJ/src/store.rs" <<'PY'
import sys;p=sys.argv[1];s=open(p).read().replace("pub fn insert(&self) -> u32 {\n        1","pub fn insert(&self) -> u32 {\n        100");open(p,"w").write(s)
PY
$B check --project "$PROJ" --data-dir "$D" 2>&1 | grep -q '1 newly stale'
ok "editing the anchored fn flags it needs_reverify" $?
(cd "$PROJ" && $B pack --data-dir "$D" --budget 600) | grep -q 'symbol changed: Store::insert'
ok "pack shows 'symbol changed: Store::insert'" $?
$B confirm "$(sqlite3 "$D/recalla.db" "select substr(uid,1,8) from memories where status='needs_reverify' limit 1")" --data-dir "$D" 2>&1 | grep -q 'Confirmed'
ok "confirm re-anchors" $?
$B check --project "$PROJ" --data-dir "$D" 2>&1 | grep -q '0 newly stale'
ok "check clean after confirm" $?

echo "== 7. MCP over stdio (both protocol eras)"
MCP=$(printf '%s\n' \
	'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}' \
	'{"jsonrpc":"2.0","method":"notifications/initialized"}' \
	'{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
	"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"remember\",\"arguments\":{\"text\":\"Run make check before every push. token=$GH\",\"kind\":\"procedure\"}}}" \
	'{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"recall","arguments":{"budget":500}}}' \
	'{"jsonrpc":"2.0","id":5,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}' |
	(cd "$PROJ" && $B mcp --data-dir "$D" --project "$PROJ"))
echo "$MCP" | python3 -c "
import sys,json
r={}
for l in sys.stdin:
    d=json.loads(l); r[d['id']]=d
assert r[1]['result']['protocolVersion']=='2025-11-25'
names=[t['name'] for t in r[2]['result']['tools']]; assert names==['recall','remember','review','why'],names
t=json.dumps(r[2]['result']); est=-(-len(t.encode())//3); print('tools/list est tokens',est); assert est<=1000
assert 'Saved' in r[3]['result']['content'][0]['text']
assert 'make check before every push' in r[4]['result']['content'][0]['text']
assert r[5]['result']['supportedVersions']==['2026-07-28','2025-11-25']
"
ok "legacy + modern handshake, 4 tools under 1k tokens, remember + recall" $?
sqlite3 "$D/recalla.db" "select text from memories" | grep -qF "$GH"
[ $? -ne 0 ]
ok "remember redacted the planted token" $?

echo "== 7b. agent-saved (MCP remember) memories are anchored and go stale too"
D2=$W/data-remember
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"remember","arguments":{"text":"`Store::other` in src/store.rs must stay cheap.","kind":"decision"}}}' | (cd "$PROJ" && $B mcp --data-dir "$D2" --project "$PROJ") >/dev/null
sqlite3 "$D2/recalla.db" "select count(*) from anchors where symbol like '%other%'" | grep -qv '^0$'; ok "remember anchored the memory to Store::other" $?
$B check --project "$PROJ" --data-dir "$D2" 2>&1 | grep -q '0 newly stale'; ok "remembered memory: check clean at first" $?
python3 - "$PROJ/src/store.rs" <<'PY'
import sys;p=sys.argv[1];s=open(p).read().replace("22","2222");open(p,"w").write(s)
PY
$B check --project "$PROJ" --data-dir "$D2" 2>&1 | grep -q '1 newly stale'; ok "editing that fn flags the remembered memory" $?

echo "== 8. hooks: settings safety and hook behaviour"
# Use the real settings as the template when present (read-only copy), else a synthetic one.
SRC=$HOME/.claude/settings.json
[ -f "$SRC" ] || { SRC=$W/synthetic-settings.json; echo '{"model":"x","hooks":{"SessionStart":[],"PreToolUse":[{"hooks":[]}],"PostToolUse":[{"hooks":[]}],"Stop":[]}}' > "$SRC"; }
cp "$SRC" "$W/settings.json"
$B hooks install --settings "$W/settings.json" >/dev/null 2>&1
ok "hooks install (on a copy of real settings)" $?
python3 - "$SRC" "$W/settings.json" <<'PY'
import json,sys
a=json.load(open(sys.argv[1]));b=json.load(open(sys.argv[2]))
assert {k:v for k,v in a.items() if k!='hooks'}=={k:v for k,v in b.items() if k!='hooks'}
for ev in ('PreToolUse','PostToolUse'): assert a.get('hooks',{}).get(ev)==b['hooks'].get(ev)
assert len(b['hooks']['SessionStart'])==len(a.get('hooks',{}).get('SessionStart',[]))+1
PY
ok "install preserved every existing hook and key" $?
$B hooks install --settings "$W/settings.json" 2>&1 | grep -qi 'already\|unchanged\|no change'
ok "install is idempotent" $?
$B hooks uninstall --settings "$W/settings.json" >/dev/null 2>&1
python3 -c "import json;assert json.load(open('$SRC'))==json.load(open('$W/settings.json'))"
ok "uninstall restores settings exactly" $?
echo '{"hooks": [broken' >"$W/bad.json"
$B hooks install --settings "$W/bad.json" >/dev/null 2>&1
[ $? = 1 ]
ok "malformed settings refused (exit 1)" $?
[ "$(cat "$W/bad.json")" = '{"hooks": [broken' ]
ok "malformed settings left untouched" $?
SS=$(echo '{"hook_event_name":"SessionStart","source":"startup","cwd":"'"$PROJ"'","session_id":"x"}' | $B hook session-start --data-dir "$D")
echo "$SS" | python3 -c "import sys,json;d=json.load(sys.stdin);assert d['hookSpecificOutput']['hookEventName']=='SessionStart' and 'Recalla memory' in d['hookSpecificOutput']['additionalContext']"
ok "session-start emits valid hookSpecificOutput" $?
OUTC=$(echo '{"hook_event_name":"Stop","transcript_path":"'"$W"'/claude/projects/-tmp-e2e/sess1.jsonl","cwd":"'"$PROJ"'"}' | CLAUDE_CONFIG_DIR="$W/claude" $B hook capture --data-dir "$D" 2>&1)
[ -z "$OUTC" ]
ok "capture: silent" $?
OUTX=$(
	echo '{"hook_event_name":"Stop","transcript_path":"/etc/passwd","cwd":"/"}' | CLAUDE_CONFIG_DIR="$W/claude" $B hook capture --data-dir "$D" 2>&1
	echo "exit=$?"
)
[ "$OUTX" = "exit=0" ]
ok "capture refuses a path outside the root, silently, exit 0" $?
echo 'garbage' | $B hook session-start --data-dir "$D" >/dev/null 2>&1
ok "garbage hook stdin exits 0" $?

echo "== 8b. markdown mirror + rebuild"
MD=$D/mirror
[ -d "$MD" ] && [ "$(stat -f '%Lp' "$MD" 2>/dev/null || stat -c '%a' "$MD")" = 700 ]; ok "mirror dir exists, mode 0700" $?
NF=$(find "$MD" -name '*.md' | wc -l | tr -d ' '); NM=$(sqlite3 "$D/recalla.db" "select count(*) from memories")
[ "$NF" = "$NM" ] && [ "$NF" -gt 0 ]; ok "one .md file per memory ($NF files, $NM memories)" $?
F1=$(find "$MD" -name '*.md' | head -1); head -3 "$F1" | grep -q 'format: 1'; ok "file starts with the format header" $?
grep -rq 'Never commit with failing tests' "$MD"; ok "memory text readable with plain grep (no engine)" $?
grep -rqF "$GH" "$MD"; [ $? -ne 0 ]; ok "no planted token in the mirror" $?
$B rebuild --from-mirror "$MD" --data-dir "$W/data2" >/dev/null 2>&1; ok "rebuild without --yes: prints plan, exit 0" $?
[ ! -e "$W/data2/recalla.db" ]; ok "rebuild without --yes changes nothing" $?
$B rebuild --from-mirror "$MD" --data-dir "$W/data2" --yes >/dev/null 2>&1; ok "rebuild --yes into a fresh data dir" $?
Q="select uid,kind,text,scope_key,status,confidence,seen_count,created_at,updated_at from memories order by uid"
diff <(sqlite3 "$D/recalla.db" "$Q") <(sqlite3 "$W/data2/recalla.db" "$Q") >/dev/null; ok "rebuilt memories identical to the original" $?
diff <(sqlite3 "$D/recalla.db" "select a.path,a.symbol,a.content_hash,coalesce(a.stale_reason,'') from anchors a order by 1,2") <(sqlite3 "$W/data2/recalla.db" "select a.path,a.symbol,a.content_hash,coalesce(a.stale_reason,'') from anchors a order by 1,2") >/dev/null; ok "rebuilt anchors identical" $?
cp -R "$MD" "$W/mirror-bad"; python3 - "$W/mirror-bad" <<'PY'
import sys,glob
f=sorted(glob.glob(sys.argv[1]+"/**/*.md",recursive=True))[0]
s=open(f).read(); i=s.index("\n---\n",5)+5; open(f,"w").write(s[:i]+"TAMPERED "+s[i:])
PY
$B rebuild --from-mirror "$W/mirror-bad" --data-dir "$W/data3" --yes >/dev/null 2>&1; [ $? = 1 ]; ok "tampered mirror file: rebuild refused (exit 1)" $?
[ ! -e "$W/data3/recalla.db" ]; ok "refused rebuild created nothing" $?
cp "$D/recalla.db" "$W/before.db"; $B rebuild --from-mirror "$MD" --data-dir "$D" --yes >/dev/null 2>&1; ok "rebuild over a live DB" $?
ls "$D"/recalla.db.bak-* >/dev/null 2>&1; ok "rebuild left a backup of the old DB" $?
[ "$(sqlite3 "$D/recalla.db" "select count(*) from events")" = "$(sqlite3 "$W/before.db" "select count(*) from events")" ]; ok "rebuild kept events/sources" $?
# Rebuild while an MCP server process is alive and holding the DB.
mkfifo "$W/mcp.in"
(cd "$PROJ" && $B mcp --data-dir "$D" --project "$PROJ" <"$W/mcp.in" >"$W/mcp.out" 2>&1) &
MCP_PID=$!
exec 3>"$W/mcp.in"
rem() { printf '%s\n' '{"jsonrpc":"2.0","id":'"$1"',"method":"tools/call","params":{"name":"remember","arguments":{"text":"'"$2"'","kind":"fact"}}}' >&3; }
rem 1 "Concurrent fact before the rebuild."
sleep 1
$B mirror --data-dir "$D" >/dev/null 2>&1
BEFORE=$(sqlite3 "$D/recalla.db" "select count(*) from memories")
$B rebuild --from-mirror "$MD" --data-dir "$D" --yes >"$W/rb.out" 2>&1; ok "rebuild while an MCP server holds the DB" $?
rem 2 "Concurrent fact after the rebuild."
sleep 1
exec 3>&-
wait $MCP_PID
[ "$(sqlite3 "$D/recalla.db" "pragma integrity_check")" = ok ]; ok "DB integrity ok after rebuild under a live MCP server" $?
[ "$(sqlite3 "$D/recalla.db" "select count(*) from memories")" = "$((BEFORE + 1))" ]; ok "MCP writes after the rebuild landed in the live DB" $?

echo "== 9. whole-DB leak scan (gitleaks over everything stored)"
sqlite3 "$D/recalla.db" "select text from events; select text from memories;" >"$W/dump.txt"
command -v gitleaks >/dev/null || echo "SKIP  gitleaks not installed (brew install gitleaks)"
GL=$(gitleaks dir "$W/dump.txt" --no-banner --redact 2>&1 | tail -1)
echo "$GL" | grep -q 'no leaks'
ok "gitleaks over stored text: no leaks" $?

echo "== 10. dashboard (recalla ui)"
UD=$W/uidata
mkdir -p "$UD"
$B ui --no-open --port 0 --data-dir "$UD" >"$W/ui.out" 2>"$W/ui.err" &
UI_PID=$!
for _ in $(seq 1 50); do grep -q 'Recalla is running' "$W/ui.out" 2>/dev/null && break; sleep 0.1; done
URL=$(sed -n 's/^Recalla is running at //p' "$W/ui.out")
BASE=${URL%%/\?*}
HP=${BASE#http://}
[ -n "$URL" ] && echo "$URL" | grep -Eq '^http://127\.0\.0\.1:[0-9]+/\?c=[0-9a-f]{64}$'; ok "ui prints a one-time code URL" $?
grep -q 'works once, for 60 seconds' "$W/ui.out"; ok "ui explains the link works once" $?
CJ=$W/cj.txt
code() { curl -s -o "${2:-/dev/null}" -w '%{http_code}' "${@:3}" "$1"; }
[ "$(code "$BASE/api/overview")" = 401 ]; ok "API without login: 401" $?
[ "$(code "$URL" /dev/null -c "$CJ" -D "$W/login.hdr")" = 302 ]; ok "login via the one-time code: 302" $?
grep -qi '^set-cookie: recalla_[0-9]*=.*; SameSite=Lax;' "$W/login.hdr"; ok "login cookie is SameSite=Lax (Strict breaks the file:// redirect page)" $?
! grep -qi 'SameSite=Strict' "$W/login.hdr"; ok "login cookie is not SameSite=Strict" $?
LOC=$(tr -d '\r' <"$W/login.hdr" | sed -n 's/^[Ll]ocation: //p')
[ "$(code "$BASE$LOC" /dev/null -b "$CJ")" = 200 ]; ok "following the 302 with the cookie reaches the dashboard: 200" $?
grep -q HttpOnly "$CJ"; ok "session cookie is HttpOnly" $?
! grep -q "$(awk -v p="${HP##*:}" '$6=="recalla_" p {print $7}' "$CJ")" "$W/ui.out"; ok "session token never printed" $?
[ "$(code "$BASE/api/overview" "$W/ov.json" -b "$CJ")" = 200 ]; ok "/api/overview: 200" $?
python3 -c "import json,sys;d=json.load(open(sys.argv[1]));assert d['memories']['total']==0 and 'version' in d" "$W/ov.json"; ok "/api/overview is valid JSON" $?
[ "$(code "$URL")" = 401 ]; ok "the code URL a second time: 401" $?
[ "$(code "$BASE/api/overview" /dev/null -b "$CJ" -H 'Host: evil.example')" = 403 ]; ok "wrong Host header: 403" $?
[ "$(code "$BASE/api/test-recall" /dev/null -b "$CJ" -X POST -H "Origin: $BASE" -d '{}')" = 403 ]; ok "POST without CSRF header: 403" $?
code "$BASE/api/session" "$W/sess.json" -b "$CJ" >/dev/null
CSRF=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['csrf'])" "$W/sess.json")
[ "$(code "$BASE/api/test-recall" "$W/tr.json" -b "$CJ" -X POST -H "Origin: $BASE" -H "X-Recalla-Csrf: $CSRF" -d '{}')" = 200 ]; ok "/api/test-recall: 200" $?
python3 -c "import json,sys;d=json.load(open(sys.argv[1]));assert 'markdown' in d and 'memories' in d" "$W/tr.json"; ok "test-recall returns a pack" $?
[ "$(code "$BASE/api/sync" "$W/sync.json" -b "$CJ" -X POST -H "Origin: $BASE" -H "X-Recalla-Csrf: $CSRF" -d '{}')" = 202 ]; ok "/api/sync starts a job: 202" $?
ST=running
for _ in $(seq 1 100); do
	code "$BASE/api/sync/status" "$W/st.json" -b "$CJ" >/dev/null
	ST=$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['state'])" "$W/st.json")
	[ "$ST" != running ] && break
	sleep 0.2
done
[ "$ST" = done ] || [ "$ST" = failed ]; ok "sync job finished (state $ST)" $?
kill "$UI_PID" 2>/dev/null
wait "$UI_PID" 2>/dev/null
echo

echo "RESULT: $P passed, $F failed"
[ "$F" = 0 ]
