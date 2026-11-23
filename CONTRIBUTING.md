# Contributing

Thanks for helping out!

1. Fork, then create a branch: `feature/<name>` or `fix/<name>`.
2. `make setup`, make your change, add tests.
3. `make check` must pass. For changes that touch hooks, MCP, redaction, anchors or the mirror, also run `make e2e` (needs `gitleaks` for the last check: `brew install gitleaks`).
4. Use Conventional Commits (`feat:`, `fix:`, `docs:` …).
5. Open a PR using the template. Keep it focused on one change.

Bugs and ideas: open an issue using the templates. Security issues: see SECURITY.md.

## Dashboard login (real browser)

Changes to `recalla ui` login, cookies or the redirect page also need `scripts/ui-browser-check.sh`. It starts the release binary with a fake opener, opens the `file://` redirect page in a real Chromium and checks that the dashboard loads. It needs tooling that is deliberately not a repo dependency, and skips with a message when it is missing:

```
mkdir /tmp/pw && cd /tmp/pw && npm i playwright-core && npx playwright-core install chromium
PLAYWRIGHT_DIR=/tmp/pw CHROME_PATH=<chromium binary> make build && scripts/ui-browser-check.sh
```

(`SHOT=file.png` saves a screenshot.)

## Landing page

`web/` is a static site (no build step). Preview it under its real subpath: `mkdir -p /tmp/serve && ln -sfn "$PWD/web" /tmp/serve/recalla && python3 -m http.server -d /tmp/serve 8080`, then open http://localhost:8080/recalla/. `bash scripts/web-check.sh` must pass (no third-party requests, assets resolve, under 100 KB). Pushes to `main` that touch `web/` deploy to GitHub Pages.

## Releasing (maintainers)

1. Move `[Unreleased]` in `CHANGELOG.md` to a dated `[X.Y.Z]` section and bump `version` in `Cargo.toml`; merge via PR once `make check` and `make e2e` are green.
2. Dry run the build matrix: Actions → release → Run workflow (builds and smoke-tests all four targets, publishes nothing).
3. `cargo publish` from the merged commit.
4. `git tag -a vX.Y.Z -m "vX.Y.Z" && git push origin vX.Y.Z`. The release workflow checks the tag against `Cargo.toml` and the CHANGELOG, builds macOS (arm64, x86_64) and Linux (x86_64, arm64) binaries, attests their provenance, and creates the GitHub release with notes from the CHANGELOG.
