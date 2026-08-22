.PHONY: build test test-e2e lint fmt check release install clean

build:
	cargo build

test:
	cargo test

# Runs the ignored lifecycle suite against a Personal Developer Instance.
# Put credentials in .env.e2e (see .env.e2e.example).
test-e2e:
	@set -a; \
	if [ -f .env.e2e ]; then . ./.env.e2e; fi; \
	set +a; \
	cargo test --test e2e -- --ignored --test-threads=1

lint:
	cargo fmt -- --check
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt

check: lint test

release:
	cargo build --release

install: check release
	cp target/release/servicenow ~/.local/bin/servicenow

clean:
	cargo clean
