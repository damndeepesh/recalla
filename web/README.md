# Recalla landing page

Plain HTML, CSS and a little vanilla JS. No build step, no dependencies, no third-party requests. This folder is the site root.

## Edit

- `index.html`: content. `404.html`: not-found page (uses `/recalla/` absolute paths because it can be served from any depth).
- `assets/app.css`: all styles and design tokens (CSS variables at the top). `assets/app.js`: tabs, copy button, theme toggle, platform detection. `assets/theme.js`: applies the saved theme before first paint.
- `assets/fonts/`: self-hosted fonts, see below.
- Keep it free of external requests. A CSP `<meta>` in each page enforces same-origin only. Page weight budget: under 100 KB of HTML, CSS and JS; images in `assets/` are not counted, so keep each small (webp).
- Releasing a new version: set `data-version` on `<html>` in `index.html` (the script rewrites the download links and version labels from it), then update the static fallback links so they work without JS: `sed -i '' 's/OLD_VERSION/X.Y.Z/g' index.html` with the old version's dots escaped (also updates `softwareVersion` in the JSON-LD).
- The terminal transcripts are real output from the release binary on synthetic projects (paths and the login token shortened). Regenerate them when output formats change.
- Dashboard screenshots (`assets/dash-<view>-<dark|light>.webp`, 1440x900, webp quality 0.8) show synthetic data only: generate Claude Code sessions from `tests/fixtures/extract/sessions.json` into a scratch dir, `recalla backfill --root <scratch> --data-dir <scratch>/data`, run a few `recalla pack --project <p> --data-dir <scratch>/data` so the ledger has packs, then `recalla ui --no-open --data-dir <scratch>/data` and capture each view in both colour schemes with a headless browser. Never use a real data dir. Only the current theme's image is loaded (the other is `display: none` with `loading="lazy"`).
- Theme: `color-scheme: light dark` with `light-dark()` tokens; `assets/theme.js` sets `data-theme` from the saved toggle choice before first paint. Terminals stay dark in both themes.

## Fonts

| File | Family | Source | Licence |
|---|---|---|---|
| `assets/fonts/bricolage-grotesque.woff2` | Bricolage Grotesque (variable, weight 500-800 used), latin subset: display type | same file as the dashboard, `src/ui/assets/fonts/` | SIL OFL 1.1, `assets/fonts/OFL-BricolageGrotesque.txt` |
| `assets/fonts/geist.woff2` | Geist (variable, weight 400-700 used), latin subset: body type | same file as the dashboard, `src/ui/assets/fonts/` | SIL OFL 1.1, `assets/fonts/OFL-Geist.txt` |
| `assets/fonts/geist-mono.woff2` | Geist Mono (variable, weight 400-600 used), latin subset: terminals and code | same file as the dashboard, `src/ui/assets/fonts/` | SIL OFL 1.1, `assets/fonts/OFL-GeistMono.txt` |

- All three are the dashboard's fonts, so the site and `recalla ui` look like one product. They are served from this origin (`font-src 'self'`), with `font-display: swap`, a latin `unicode-range`, and metric-matched local fallbacks (`size-adjust`, `ascent-override`) in `app.css` so the swap does not shift layout. Bricolage Grotesque and Geist are preloaded.
- To update one: update it in `src/ui/assets/fonts/` first, then copy the woff2 and its OFL file here, keeping the file names. `scripts/web-check.sh` fails on any font that is not same-origin woff2, not referenced by an `@font-face`, over 64 KB (120 KB total), or missing an OFL licence file.

## Preview under the real subpath

The site is served at `https://deepeshgupta.dev/recalla/`, so test it under `/recalla/`:

```sh
mkdir -p /tmp/recalla-preview && ln -sfn "$PWD" /tmp/recalla-preview/recalla
cd /tmp/recalla-preview && python3 -m http.server 8765
# open http://localhost:8765/recalla/
```

All asset URLs are relative, so it also works at any other path.

## Deploy

A GitHub Pages workflow publishes `web/` on pushes to `web/**` at `https://damndeepesh.github.io/recalla/`. The portfolio site at deepeshgupta.dev rewrites `/recalla/*` to that origin.

## Regenerate `assets/og.png`

Edit `tools/og.html` (it uses the self-hosted fonts), serve the folder as above, and screenshot it at exactly 1200x630 with any headless browser (for example Playwright: `page.setViewportSize({width:1200,height:630})`, open `/recalla/tools/og.html`, `page.screenshot({path:'assets/og.png'})`).

## Publishing to deepeshgupta.dev/recalla

The page is deployed to GitHub Pages (`https://damndeepesh.github.io/recalla/`) by `.github/workflows/pages.yml`. `deepeshgupta.dev/recalla` is served by the portfolio site, which carries a copy under `public/recalla/` (a proxy rewrite is not reliably supported by its Cloudflare adapter). After changing `web/`, refresh that copy from a checkout of this repo next to a checkout of the portfolio:

```bash
rsync -a --delete \
  --exclude tools --exclude README.md --exclude 404.html --exclude .nojekyll \
  --exclude robots.txt --exclude sitemap.xml \
  web/ ../portfolio/public/recalla/
```

Then commit and merge that change in the portfolio repo; Cloudflare redeploys it.
