help:
    @just --list

clippy-flags := "\
-D warnings \
-D clippy::all \
-D clippy::allow_attributes \
-D clippy::dbg_macro \
-D clippy::print_stderr \
-D clippy::await_holding_lock \
-D clippy::large_futures \
-D clippy::todo \
-D clippy::indexing_slicing \
-D clippy::future_not_send \
-D clippy::significant_drop_tightening \
-D clippy::panic_in_result_fn \
-D clippy::unwrap_used \
-D clippy::expect_used \
-D clippy::unimplemented \
-D clippy::unused_async \
-D clippy::needless_collect \
-D clippy::large_stack_arrays \
-D clippy::filetype_is_file \
-D clippy::manual_let_else \
-D clippy::readonly_write_lock \
-D clippy::mutex_atomic \
-D clippy::map_err_ignore \
-D clippy::iter_over_hash_type \
-D clippy::panic \
"

update-rust-deps:
    cargo upgrade
    cargo update
    just generate-cargo-nix

update-rust-deps-full:
    cargo upgrade --incompatible
    cargo update
    just generate-cargo-nix

generate-cargo-nix:
    crate2nix generate

test:
    cargo nextest run

clean:
    cargo clean

build:
    cargo build --locked -p kraai-tui -p kraai-nushell-runtime --bins

[positional-arguments]
run *args: build
    cargo run --locked -p kraai-tui -- "$@"

dev:
    cargo watch -w crates -c

format:
    nix fmt -- -c

lint:
    cargo clippy --workspace --all-targets --all-features -- {{ clippy-flags }}

lint-fix:
    cargo clippy --workspace --all-targets --all-features --fix -- {{ clippy-flags }}

lint-fix-dirty:
    cargo clippy --workspace --all-targets --all-features --fix --allow-dirty -- {{ clippy-flags }}

test-harbor:
    uv run --locked --python python3.12 --project evals/harbor python -m unittest discover -s evals/harbor/tests
build-node:
    node packages/runtime/scripts/build.mjs

check-node:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" != Linux ]]; then exit 0; fi
    "{{ just_executable() }}" build-node
    cd packages/runtime
    npm run typecheck
    npm test

check: generate-cargo-nix format lint test test-harbor check-node

eval *args:
    nix run .#kraai-eval -- {{ args }}
