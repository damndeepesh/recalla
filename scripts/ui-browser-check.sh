#!/usr/bin/env bash
# Real-browser regression test for the `recalla ui` login hand-off: the CLI
# opens a file:// redirect page, which must end on a working dashboard.
# Optional tooling (no repo dependency): node + playwright-core + a Chromium.
#   PLAYWRIGHT_DIR=<dir with node_modules/playwright-core>  CHROME_PATH=<chrome binary>
#   RECALLA_BIN=target/release/recalla  SHOT=<screenshot.png>
set -u
cd "$(dirname "$0")/.."
B=${RECALLA_BIN:-target/release/recalla}
[ -x "$B" ] || B=target/debug/recalla
command -v node >/dev/null || { echo "SKIP  node not installed"; exit 0; }
PW=${PLAYWRIGHT_DIR:-.}
(cd "$PW" && node -e 'require("playwright-core")') 2>/dev/null ||
	{ echo "SKIP  playwright-core not found (set PLAYWRIGHT_DIR, see CONTRIBUTING.md)"; exit 0; }
[ -x "$B" ] || { echo "no recalla binary; run make build"; exit 1; }
B=$(cd "$(dirname "$B")" && pwd)/$(basename "$B")

W=$(mktemp -d)
trap 'kill "${PID:-0}" 2>/dev/null; rm -r "$W"' EXIT
mkdir -p "$W/bin" "$W/data"
# A fake opener records the file the CLI hands to the browser (the real code path).
for o in open xdg-open; do
	printf '#!/bin/sh\nprintf %%s "$1" > "%s/opened"\n' "$W" >"$W/bin/$o"
	chmod +x "$W/bin/$o"
done
PATH="$W/bin:$PATH" "$B" ui --port 0 --data-dir "$W/data" >"$W/out" 2>"$W/err" &
PID=$!
for _ in $(seq 1 100); do [ -s "$W/opened" ] && grep -q 'Recalla is running' "$W/out" && break; sleep 0.1; done
URL=$(sed -n 's/^Recalla is running at //p' "$W/out")
[ -s "$W/opened" ] && [ -n "$URL" ] || { echo "FAIL  ui did not start"; cat "$W/err"; exit 1; }
cd "$PW" && node "$OLDPWD/scripts/ui-browser-check.mjs" "$(cat "$W/opened")" "$URL" "${SHOT:--}"
