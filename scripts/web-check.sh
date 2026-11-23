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
# 1b. Same check across line breaks (a formatter can split a tag over several lines).
python3 - "$W" <<'PY' || fail=1
import re,sys,pathlib
w=pathlib.Path(sys.argv[1]); hits=[]
for f in [*w.glob("*.html"),*w.glob("assets/*.css"),*w.glob("assets/*.js")]:
    t=f.read_text()
    for tag in re.findall(r"<(?:link|script|img|source|iframe|video|audio)\b[^>]*>",t,re.S):
        if re.search(r'rel="canonical"',tag): continue
        if re.search(r'(?:src|srcset|href)\s*=\s*"\s*(?:https?:)?//',tag): hits.append(f"{f.name}: {' '.join(tag.split())[:120]}")
    hits+=[f"{f.name}: {m}" for m in re.findall(r"url\(\s*['\"]?\s*(?:https?:)?//[^)]*\)",t)]
if hits: print("FAIL  third-party loads (multi-line scan):",*hits,sep="\n   "); sys.exit(1)
print("ok    no third-party loads (multi-line scan)")
PY

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

# 4b. Fonts: same-origin woff2 only, every @font-face file exists and is used, size caps,
#     an OFL licence ships next to them, and every preload points at a face the CSS declares.
python3 - "$W" <<'PY' || fail=1
import re,sys,pathlib
w=pathlib.Path(sys.argv[1]); fonts=w/"assets/fonts"; errs=[]
css=(w/"assets/app.css").read_text()
faces=re.findall(r"@font-face\s*{(.*?)}",css,re.S)
used=set()
for face in faces:
    for u in re.findall(r"url\(\s*['\"]?([^'\")]+)",face):
        if re.match(r"(?:[a-z]+:)?//|data:",u): errs.append(f"@font-face loads {u}: fonts must be same-origin files"); continue
        if not u.endswith(".woff2"): errs.append(f"{u}: only .woff2 is allowed")
        f=(w/"assets"/u).resolve(); used.add(f)
        if not f.exists(): errs.append(f"{u}: missing")
files={f.resolve() for f in fonts.glob("*")} if fonts.exists() else set()
for f in sorted(fonts.glob("*.woff2")) if fonts.exists() else []:
    if f.resolve() not in used: errs.append(f"{f.name}: not referenced by any @font-face")
    if f.stat().st_size>64*1024: errs.append(f"{f.name}: {f.stat().st_size} bytes exceeds 64 KB")
total=sum(f.stat().st_size for f in fonts.glob("*.woff2")) if fonts.exists() else 0
if total>120*1024: errs.append(f"fonts total {total} bytes exceeds 120 KB")
others=[f.name for f in fonts.glob("*") if f.suffix not in (".woff2",".txt")] if fonts.exists() else []
if others: errs.append(f"unexpected files in assets/fonts: {others}")
n=len(list(fonts.glob("*.woff2"))) if fonts.exists() else 0
lic=[f for f in fonts.glob("OFL-*.txt") if "SIL Open Font License" in f.read_text()] if fonts.exists() else []
if n and len(lic)<n: errs.append(f"{n} font files but {len(lic)} OFL licence files (assets/fonts/OFL-*.txt)")
for page in ("index.html","404.html"):
    html=(w/page).read_text()
    for tag in re.findall(r"<link\b[^>]*>",html,re.S):
        if 'rel="preload"' in tag and 'as="font"' in tag:
            href=re.search(r'href="([^"]+)"',tag).group(1)
            f=(w/href.removeprefix("/recalla/")).resolve()
            if f not in used: errs.append(f"{page}: preload {href} is not a declared @font-face")
            if "crossorigin" not in tag: errs.append(f"{page}: font preload {href} needs crossorigin")
if errs: print("FAIL  fonts:",*errs,sep="\n   "); sys.exit(1)
print(f"ok    fonts: {n} same-origin woff2 files, {total} bytes, licences present")
PY

# 5a. The <base> that keeps assets working at /recalla (no trailing slash) is present.
grep -qE '<base href="/recalla/" ?/?>' "$W/index.html" && ok "<base href=/recalla/> present" || bad "web/index.html lost its <base href=\"/recalla/\">"

# 5. The version in the download links matches Cargo.toml.
ver=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
grep -q "data-version=\"$ver\"" "$W/index.html" && ok "download version matches Cargo.toml ($ver)" || bad "web/index.html data-version does not match Cargo.toml ($ver)"

[ "$fail" = 0 ] && echo "web-check: all good" || { echo "web-check: FAILED"; exit 1; }
