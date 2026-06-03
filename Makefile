.DEFAULT_GOAL := help

.PHONY: help build test fmt fmt-check clippy lint check clean release-build \
        ci-local ci-local-fast install-hooks uninstall-hooks

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
	@echo "  ci-local      run the FULL local CI-equivalent suite (mirrors ci.yaml; incl. musl + coverage)"
	@echo "  ci-local-fast ci-local without the musl leg + coverage (quick iteration)"
	@echo "  install-hooks install the git pre-push hook (blocks pushing red work)"
	@echo "  uninstall-hooks  remove the git pre-push hook"

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

# Local CI-equivalent gate (mirrors .github/workflows/ci.yaml). The pre-push
# hook runs `ci-local`; failures block the push so red never reaches paid CI.
ci-local:
	utils/ci/pre-push.sh

ci-local-fast:
	utils/ci/pre-push.sh --fast

# Point git at the tracked hooks dir so utils/git-hooks/pre-push runs on push.
install-hooks:
	git config core.hooksPath utils/git-hooks
	@echo "Installed: git pre-push hook -> utils/ci/pre-push.sh (full CI-equivalent suite)."
	@echo "Bypass once with: git push --no-verify"

uninstall-hooks:
	git config --unset core.hooksPath || true
	@echo "Removed core.hooksPath; git uses .git/hooks again."
