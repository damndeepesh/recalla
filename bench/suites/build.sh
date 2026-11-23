#!/usr/bin/env bash
# Rebuilds every suite from sources.txt. Needs network once, to clone each
# repo (full history, no checkout) into $REPOS; mining itself is offline.
#   REPOS=/some/scratch/dir suites/build.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
REPOS=${REPOS:?set REPOS to a scratch directory for the clones}
bin=${MEMBENCH:-$here/../target/release/membench}
mkdir -p "$REPOS"
grep -v '^#' "$here/sources.txt" | while read -r name url license head; do
  [ -d "$REPOS/$name" ] || git clone -q --no-checkout "$url.git" "$REPOS/$name"
  for epoch in 1:0.40 2:0.62 3:0.80; do
    e=${epoch%%:*}; t0=${epoch##*:}
    "$bin" mine --repo "$REPOS/$name" --name "$name-e$e" --url "$url" --license "$license" \
      --head "$head" --t0 "$t0" --out "$here/$name-e$e.json"
  done
done
