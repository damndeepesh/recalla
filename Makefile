.PHONY: setup dev fmt check build

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
