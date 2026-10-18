.PHONY: help fmt fmt-check lint test build check

help:
	@printf '%s\n' 'make check      Format check, lint, test, build, test again' 'make test       Run the real Rust test suite offline' 'make build      Build the Rust workspace offline' 'make fmt        Format Rust source' 'make lint       Check Rust warnings'

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets --offline -- -D warnings

test:
	cargo test --workspace --offline

build:
	cargo build --workspace --offline

check:
	$(MAKE) fmt-check
	$(MAKE) lint
	$(MAKE) test
	$(MAKE) build
	$(MAKE) test
