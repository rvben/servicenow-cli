.PHONY: build test lint fmt check release install clean

build:
	cargo build

test:
	cargo test

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

