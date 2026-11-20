#!/bin/sh
# fmt, clippy and tests of the workspace, then the e2e latency and memory budgets of a
# release build, inside the dev container
podman build -q -t compcore-dev - <"$(dirname "$0")/../Containerfile" &&
	podman run --rm -v "${1:-$PWD}":/src:Z \
		-v compcore-cargo-registry:/usr/local/cargo/registry \
		-v compcore-cargo-git:/usr/local/cargo/git \
		compcore-dev sh -c 'cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo test --release -p compcore --test e2e'
