# Contributing

Thanks for helping out!

1. Fork, then create a branch: `feature/<name>` or `fix/<name>`.
2. `make setup`, make your change, add tests.
3. `make check` must pass. For changes that touch hooks, MCP, redaction, anchors or the mirror, also run `make e2e` (needs `gitleaks` for the last check: `brew install gitleaks`).
4. Use Conventional Commits (`feat:`, `fix:`, `docs:` …).
5. Open a PR using the template. Keep it focused on one change.

Bugs and ideas: open an issue using the templates. Security issues: see SECURITY.md.
