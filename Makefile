.PHONY: help fmt fmt-check lint test test-node build check

help:
	@printf '%s\n' 'make check      Format check, lint, test, build, test again' 'make test       Run the real Rust and Node test suites' 'make test-node  Run first-party Node tests' 'make build      Build the Rust workspace offline' 'make fmt        Format Rust source' 'make lint       Check Rust warnings'

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets --offline -- -D warnings

test:
	cargo test --workspace --offline
	$(MAKE) test-node

test-node:
	node --test --test-timeout=15000 \
		scripts/rcli/host.codec.test.mjs \
		scripts/rcli/host.messages.test.mjs \
		scripts/rcli/host.turns.test.mjs \
		scripts/rcli/host.entry.test.mjs \
		scripts/rcli/build-closure.test.mjs \
		apps/desktop/test/model-access.test.mjs \
		apps/desktop/test/native/refresh-barrier.test.mjs \
		apps/desktop/test/native/config-registration.test.mjs \
		apps/desktop/test/native/baseline-reload.test.mjs

build:
	cargo build --workspace --offline

check:
	$(MAKE) fmt-check
	$(MAKE) lint
	$(MAKE) test
	$(MAKE) build
	$(MAKE) test
