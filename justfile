help:
    @just --list

clean:
    cargo clean
    pnpm clean
    pnpm -r clean

build-bindings:
    cd crates/ts-bindings && pnpm build

build-bindings-debug:
    cd crates/ts-bindings && pnpm build:debug

test-bindings:
    cd crates/ts-bindings && pnpm test

clean-bindings:
    cd crates/ts-bindings && pnpm clean

dev-desktop:
    cd apps/agent-desktop && pnpm dev

build-desktop:
    cd apps/agent-desktop && pnpm build

build-desktop-pack:
    cd apps/agent-desktop && pnpm build:unpack

typecheck-desktop:
    cd apps/agent-desktop && pnpm typecheck

install-deps:
    pnpm install

build-all: build-bindings build-desktop

dev-all: build-bindings dev-desktop

test-all: test-bindings typecheck-desktop

format:
    nix fmt -- -c

lint:
    pnpm lint
    cargo clippy -- -D warnings

check: format lint test-all

reset: clean
    pnpm install

setup: install-deps build-bindings

# Get the current system (e.g., "x86_64-linux", "aarch64-linux")

[private]
_system := `nix eval --raw --impure --expr 'builtins.currentSystem'`

localCI:
    nix flake check
    nix build .#devShells.{{ _system }}.default --no-link
