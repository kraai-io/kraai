help:
    @just --list

update-rust-deps:
    cargo upgrade
    cargo update

update-rust-deps-full:
    cargo upgrade --incompatible
    cargo update

clean:
    cargo clean
    pnpm clean
    pnpm -r clean
    rm -rf crates/agent-ts-bindings/dist crates/agent-ts-bindings/index.js crates/agent-ts-bindings/index.d.ts crates/agent-ts-bindings/browser.js
    rm -rf crates/agent-ts-bindings/*.node
    rm -rf dist/ releases/

build-bindings:
    cd crates/agent-ts-bindings && pnpm build

build-bindings-debug:
    cd crates/agent-ts-bindings && pnpm build:debug

test-bindings:
    cd crates/agent-ts-bindings && pnpm test

clean-bindings:
    cd crates/agent-ts-bindings && pnpm clean

dev-nohr: build-bindings-debug dev-desktop

dev:
    cargo watch -w crates -c -s "just dev-nohr"

dev-rust:
    cargo watch -w crates -c

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

test-all: test-bindings typecheck-desktop

format:
    nix fmt -- -c

lint:
    pnpm lint
    cargo clippy -- -D warnings

check: format lint test-all

reset: clean
    pnpm install

setup: install-deps

# Get the current system (e.g., "x86_64-linux", "aarch64-linux")

[private]
_system := `nix eval --raw --impure --expr 'builtins.currentSystem'`

localCI:
    nix flake check
    nix build .#devShells.{{ _system }}.default --no-link

# Distribution builds - Fresh environment, cross-compilation ready
# All packages go to releases/ directory

# Detect current platform for native builds
[private]
dist-detect-target:
    #!/usr/bin/env bash
    case "$(uname -s)" in
        Linux)
            case "$(uname -m)" in
                x86_64) echo "x86_64-unknown-linux-gnu" ;;
                aarch64) echo "aarch64-unknown-linux-gnu" ;;
                *) echo "unsupported" ;;
            esac
            ;;
        Darwin)
            case "$(uname -m)" in
                x86_64) echo "x86_64-apple-darwin" ;;
                arm64) echo "aarch64-apple-darwin" ;;
                *) echo "unsupported" ;;
            esac
            ;;
        MINGW*|MSYS*|CYGWIN*)
            case "$(uname -m)" in
                x86_64) echo "x86_64-pc-windows-msvc" ;;
                *) echo "unsupported" ;;
            esac
            ;;
        *) echo "unsupported" ;;
    esac

# Install all dependencies fresh
dist-setup:
    pnpm install
    mkdir -p releases

# Build bindings for specific target (cross-compilation ready)

# TARGET examples: x86_64-unknown-linux-gnu, x86_64-apple-darwin, x86_64-pc-windows-msvc
dist-build-bindings TARGET="native":
    #!/usr/bin/env bash
    if [ "{{ TARGET }}" = "native" ]; then
        TARGET_TRIPLE=$(just dist-detect-target)
        if [ "$TARGET_TRIPLE" = "unsupported" ]; then
            echo "Unsupported platform"
            exit 1
        fi
        echo "Building bindings for native target: $TARGET_TRIPLE"
        cd crates/agent-ts-bindings && pnpm napi build --platform --release --target "$TARGET_TRIPLE"
    else
        echo "Building bindings for target: {{ TARGET }}"
        cd crates/agent-ts-bindings && pnpm napi build --platform --release --target "{{ TARGET }}"
    fi

# Build electron app (typecheck + vite build)
dist-build-app:
    cd apps/agent-desktop && pnpm build

# Package the app for current platform (unpackaged, for testing)
dist-unpack: dist-setup (dist-build-bindings "native") dist-build-app
    cd apps/agent-desktop && pnpm build:unpack

# Linux distribution build
# Supports: x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-unknown-linux-musl

# Usage: just dist-linux (auto-detect) or just dist-linux x86_64-unknown-linux-gnu
dist-linux TARGET="native": dist-setup (dist-build-bindings TARGET) dist-build-app
    #!/usr/bin/env bash
    if [ "{{ TARGET }}" = "native" ]; then
        TARGET_TRIPLE=$(just dist-detect-target)
    else
        TARGET_TRIPLE="{{ TARGET }}"
    fi
    echo "Packaging for Linux ($TARGET_TRIPLE)..."
    (cd apps/agent-desktop && pnpm electron-builder --linux --config)
    echo "Copying artifacts to releases/$TARGET_TRIPLE/"
    mkdir -p "releases/$TARGET_TRIPLE"
    cp -r apps/agent-desktop/dist/* "releases/$TARGET_TRIPLE/" 2>/dev/null || true

# macOS distribution build
# Supports: x86_64-apple-darwin, aarch64-apple-darwin

# Usage: just dist-mac (auto-detect) or just dist-mac aarch64-apple-darwin
dist-mac TARGET="native": dist-setup (dist-build-bindings TARGET) dist-build-app
    #!/usr/bin/env bash
    if [ "{{ TARGET }}" = "native" ]; then
        TARGET_TRIPLE=$(just dist-detect-target)
    else
        TARGET_TRIPLE="{{ TARGET }}"
    fi
    echo "Packaging for macOS ($TARGET_TRIPLE)..."
    (cd apps/agent-desktop && pnpm electron-builder --mac --config)
    echo "Copying artifacts to releases/$TARGET_TRIPLE/"
    mkdir -p "releases/$TARGET_TRIPLE"
    cp -r apps/agent-desktop/dist/* "releases/$TARGET_TRIPLE/" 2>/dev/null || true

# Windows distribution build
# Supports: x86_64-pc-windows-msvc, aarch64-pc-windows-msvc, i686-pc-windows-msvc

# Usage: just dist-win (auto-detect) or just dist-win x86_64-pc-windows-msvc
dist-win TARGET="native": dist-setup (dist-build-bindings TARGET) dist-build-app
    #!/usr/bin/env bash
    if [ "{{ TARGET }}" = "native" ]; then
        TARGET_TRIPLE=$(just dist-detect-target)
    else
        TARGET_TRIPLE="{{ TARGET }}"
    fi
    echo "Packaging for Windows ($TARGET_TRIPLE)..."
    (cd apps/agent-desktop && pnpm electron-builder --win --config)
    echo "Copying artifacts to releases/$TARGET_TRIPLE/"
    mkdir -p "releases/$TARGET_TRIPLE"
    cp -r apps/agent-desktop/dist/* "releases/$TARGET_TRIPLE/" 2>/dev/null || true
