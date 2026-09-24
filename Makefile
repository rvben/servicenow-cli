.PHONY: build test test-e2e lint lint-md fmt check audit msrv release install clean release-patch release-minor release-major

build:
	cargo build

test:
	cargo test --locked --all-targets

# Runs the ignored lifecycle suite against a Personal Developer Instance.
# Put credentials in .env.e2e (see .env.e2e.example).
test-e2e:
	@set -a; \
	if [ -f .env.e2e ]; then . ./.env.e2e; fi; \
	set +a; \
	cargo test --test e2e -- --ignored --test-threads=1

lint: lint-md
	cargo fmt -- --check
	cargo clippy --locked --all-targets -- -D warnings

lint-md:
	uv run --locked --only-group dev rumdl check $$(git ls-files '*.md')

fmt:
	cargo fmt

# Not part of `check`: it needs network access to fetch the RustSec advisory
# database, and check runs locally (and as the vership release gate) where
# that access is not guaranteed. Run it explicitly, or via CI's security job.
audit:
	@command -v cargo-audit >/dev/null 2>&1 || cargo install cargo-audit --version 0.22.2 --locked
	cargo audit

# Reads the declared MSRV from Cargo.toml so it is not hardcoded twice.
msrv:
	$(eval MSRV := $(shell sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml))
	rustup toolchain install $(MSRV) --profile minimal >/dev/null
	cargo +$(MSRV) check --locked --all-targets

check: lint test

release:
	cargo build --release

install: check release
	cp target/release/servicenow ~/.local/bin/servicenow

clean:
	cargo clean

release-patch:
	vership bump patch

release-minor:
	vership bump minor

release-major:
	vership bump major
