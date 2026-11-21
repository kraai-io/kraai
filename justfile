help:
    @just --list

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
    cargo nextest run --all-features

clean:
    cargo clean

build:
    cargo build --locked -p kraai-tui --bin kraai

build-acp:
    cargo build --locked -p kraai-acp --bin kraai-acp

build-perf:
    cargo build --locked --release -p kraai-perf

[positional-arguments]
perf *args:
    cargo run --locked --release -p kraai-perf -- "$@"

[positional-arguments]
profile *args:
    cargo run --locked --profile profiling -p kraai-perf -- profile "$@"

[positional-arguments]
run *args: build
    cargo run --locked -p kraai-tui -- "$@"

dev:
    cargo watch -w crates -c

format:
    nix fmt -- -c

lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

lint-fix:
    cargo clippy --workspace --all-targets --all-features --fix -- -D warnings

lint-fix-dirty:
    cargo clippy --workspace --all-targets --all-features --fix --allow-dirty -- -D warnings

test-harbor:
    uv run --locked --python python3.12 --project evals/harbor python -m unittest discover -s evals/harbor/tests
build-node:
    node packages/runtime/scripts/build.mjs

[script('bash')]
check-node test_script="test":
    set -euo pipefail
    "{{ just_executable() }}" build-node
    cd packages/runtime
    npm run typecheck
    npm run "{{ test_script }}"

# Builds a Vite app in packages/<package>, such as eval-viewer or site.
[script('bash')]
build-web package:
    set -euo pipefail
    cd "packages/{{ package }}"
    npm ci --no-audit --no-fund
    npm run build

[script('bash')]
check-web package: (build-web package)
    set -euo pipefail
    cd "packages/{{ package }}"
    npm run typecheck
    npm test

check: generate-cargo-nix format lint test test-harbor check-node (check-web "eval-viewer") (check-web "site")

eval *args:
    nix run .#kraai-eval -- {{ args }}

eval-view: (eval "view" "--no-open" "--port" "3928")
