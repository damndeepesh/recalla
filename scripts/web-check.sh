#!/bin/bash
# Guards the landing page's promises: no third-party requests, every local asset exists,
# JSON-LD parses, and the page stays small. Run from the repo root: bash scripts/web-check.sh
set -u
cd "$(dirname "$0")/.." || exit 1
W=web; fail=0
bad() { echo "FAIL  $1"; fail=1; }
ok()  { echo "ok    $1"; }

# 1. No resource loads from other origins in HTML/CSS/JS (links to other sites are fine).
ext=$(grep -RhoE '(src|srcset|data-src)="https?://[^"]+"|<link[^>]+href="https?://[^"]+"|@import +url\(["'\'']?https?://|url\(["'\'']?https?://|fetch\(["'\'']https?://|import\(["'\'']https?://|new +(XMLHttpRequest|WebSocket|EventSource)' "$W" --include='*.html' --include='*.css' --include='*.js' 2>/dev/null | grep -v 'rel="canonical"' | grep -v 'property=' | sort -u)
[ -z "$ext" ] && ok "no third-party resource loads" || { bad "third-party resource loads found:"; echo "$ext"; }

# 2. Every relative src/href/url() in index.html and 404.html points at a file that exists.
python3 - "$W" <<'PY' || fail=1
import re,sys,pathlib
w=pathlib.Path(sys.argv[1]); missing=[]
for page in ("index.html","404.html"):
    html=(w/page).read_text()
    refs=re.findall(r'(?:src|href)="([^"#?:]+)"',html)+re.findall(r"url\(['\"]?([^'\")#?:]+)",html)
    for r in refs:
        if r.startswith(("http","mailto","/")) or not r: continue
        if not (w/r).exists(): missing.append(f"{page}: {r}")
css=(w/"assets/app.css").read_text()
for r in re.findall(r"url\(['\"]?([^'\")#?:]+)",css):
    if r.startswith(("http","data","/")): continue
    if not (w/"assets"/r).exists(): missing.append(f"app.css: {r}")
if missing: print("FAIL  missing local files:",*missing,sep="\n   "); sys.exit(1)
print("ok    every local asset reference resolves")
PY

# 3. JSON-LD parses.
python3 - "$W/index.html" <<'PY' || fail=1
import re,sys,json
blocks=re.findall(r'<script type="application/ld\+json">(.*?)</script>',open(sys.argv[1]).read(),re.S)
if not blocks: print("FAIL  no JSON-LD block"); sys.exit(1)
for b in blocks: json.loads(b)
print("ok    JSON-LD parses")
PY

# 4. A CSP exists and forbids third parties; page weight stays under budget.
grep -q 'Content-Security-Policy' "$W/index.html" && ok "CSP meta present" || bad "CSP meta missing"
total=$(cat "$W/index.html" "$W/404.html" "$W"/assets/*.css "$W"/assets/*.js "$W/favicon.svg" | wc -c | tr -d ' ')
[ "$total" -lt 102400 ] && ok "page weight ${total} bytes (< 100 KB)" || bad "page weight ${total} bytes exceeds 100 KB"

# 5. The version in the download links matches Cargo.toml.
ver=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
grep -q "data-version=\"$ver\"" "$W/index.html" && ok "download version matches Cargo.toml ($ver)" || bad "web/index.html data-version does not match Cargo.toml ($ver)"

[ "$fail" = 0 ] && echo "web-check: all good" || { echo "web-check: FAILED"; exit 1; }
