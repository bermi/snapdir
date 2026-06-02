.DEFAULT_GOAL := help

.PHONY: help build test fmt fmt-check clippy lint check clean release-build

help: ## Show available targets
	@echo "snapdir — cargo convenience wrapper"
	@echo
	@echo "Targets:"
	@echo "  build         cargo build --workspace --locked"
	@echo "  test          cargo test --workspace --locked"
	@echo "  fmt           cargo fmt --all"
	@echo "  fmt-check     cargo fmt --all --check"
	@echo "  clippy/lint   cargo clippy (warnings as errors)"
	@echo "  check         fmt-check + clippy + test (the CI bar)"
	@echo "  clean         cargo clean"
	@echo "  release-build cargo build --release --workspace --locked"

build:
	cargo build --workspace --locked

test:
	cargo test --workspace --locked

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all --check

clippy:
	cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

lint: clippy

check: fmt-check clippy test

clean:
	cargo clean

# Tagged releases are produced by .github/workflows/release.yml (cargo-dist), not this target.
release-build:
	cargo build --release --workspace --locked
