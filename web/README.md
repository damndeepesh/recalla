# Recalla landing page

Plain HTML, CSS and a little vanilla JS. No build step, no dependencies, no third-party requests. This folder is the site root.

## Edit

- `index.html`: content. `404.html`: not-found page (uses `/recalla/` absolute paths because it can be served from any depth).
- `assets/app.css`: all styles and design tokens (CSS variables at the top). `assets/app.js`: tabs, copy button, theme toggle, platform detection. `assets/theme.js`: applies the saved theme before first paint.
- Keep it free of external requests. A CSP `<meta>` in each page enforces same-origin only. Page weight budget: under 100 KB of HTML, CSS and JS; images in `assets/` are not counted, so keep each small (webp).
- Releasing a new version: set `data-version` on `<html>` in `index.html` (the script rewrites the download links and version labels from it), then update the static fallback links so they work without JS: `sed -i '' 's/OLD_VERSION/X.Y.Z/g' index.html` with the old version's dots escaped (also updates `softwareVersion` in the JSON-LD).
- The terminal demo is real output from the release binary on a synthetic project. Regenerate it when output formats change.

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

Edit `tools/og.html`, serve the folder as above, and screenshot it at exactly 1200x630 with any headless browser (for example Playwright: `page.setViewportSize({width:1200,height:630})`, open `/recalla/tools/og.html`, `page.screenshot({path:'assets/og.png'})`).

## Publishing to deepeshgupta.dev/recalla

The page is deployed to GitHub Pages (`https://damndeepesh.github.io/recalla/`) by `.github/workflows/pages.yml`. `deepeshgupta.dev/recalla` is served by the portfolio site, which carries a copy under `public/recalla/` (a proxy rewrite is not reliably supported by its Cloudflare adapter). After changing `web/`, refresh that copy from a checkout of this repo next to a checkout of the portfolio:

```bash
rsync -a --delete \
  --exclude tools --exclude README.md --exclude 404.html --exclude .nojekyll \
  --exclude robots.txt --exclude sitemap.xml \
  web/ ../portfolio/public/recalla/
```

Then commit and merge that change in the portfolio repo; Cloudflare redeploys it.
