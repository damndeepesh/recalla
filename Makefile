.PHONY: setup dev fmt check build perf secrets e2e bench bench-check live

setup:
	rustup show active-toolchain
	cargo fetch

dev:
	cargo run -- --version

fmt:
	cargo fmt --all

comma := ,

# fmt-check + lint + typecheck + tests. Must pass before push (enforced by hook + CI).
check:
	cargo fmt --all -- --check
	cargo clippy --all-targets --locked -- -D warnings
	cargo test --all-targets --locked

build:
	cargo build --release --locked

# SPEC perf targets on synthetic data (slow; not part of check).
perf:
	cargo test --release --locked --test perf -- --ignored --nocapture --test-threads=1

# Scan full git history for secrets (needs gitleaks: brew install gitleaks). Also runs in CI.
secrets:
	gitleaks git . --redact --no-banner

# End-to-end functional checklist against the release binary (planted secrets, hooks, MCP, anchors, mirror).
e2e: build
	bash scripts/e2e.sh target/release/recalla

# Live round trip with the real claude/codex CLIs and installed hooks (scratch data dir; not in CI).
live:
	bash scripts/live.sh

# Benchmark crate checks (separate cargo workspace in bench/).
bench-check:
	cd bench && cargo fmt --all -- --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked

# Benchmark run: baselines + bundled fake MCP server; add REPOS=<clones> for real worktrees,
# RECALLA=1 to include Recalla (builds the release binary).
bench:
	cd bench && cargo build --release --locked
	@if [ -n "$(RECALLA)" ]; then cargo build --release --locked; fi
	cd bench && ./target/release/membench run --suite suites/*-e?.json \
		--adapters none,file,grep,dump,mcp$(if $(RECALLA),$(comma)recalla$(comma)recalla-backfill,) \
		--mcp-command "./target/release/membench fake-mcp" --mcp-name fake-mcp \
		$(if $(RECALLA),--recalla ../target/release/recalla,) \
		$(if $(REPOS),--repos $(REPOS),) \
		--split holdout --report report.md --out raw.json
	@sed -n 1,16p bench/report.md
