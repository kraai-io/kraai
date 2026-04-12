help:
    @just --list

update-rust-deps:
    cargo upgrade
    cargo update
    cargo hakari generate

update-rust-deps-full:
    cargo upgrade --incompatible
    cargo update
    cargo hakari generate

test:
    cargo nextest run

clean:
    cargo clean

dev:
    cargo watch -w crates -c

format:
    nix fmt -- -c

lint:
    cargo clippy --all-targets -- -D warnings

check: format lint test

# Get the current system (e.g., "x86_64-linux", "aarch64-linux")

[private]
_system := `nix eval --raw --impure --expr 'builtins.currentSystem'`

localCI:
    nix flake check
    nix build .#devShells.{{ _system }}.default --no-link
