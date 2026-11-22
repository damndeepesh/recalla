.PHONY: setup dev fmt check build perf secrets

setup:
	rustup show active-toolchain
	cargo fetch

dev:
	cargo run -- --version

fmt:
	cargo fmt --all

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
