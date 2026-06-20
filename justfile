help:
    @just --list

clippy-flags := "\
-D warnings \
"

# -D clippy::all \
# -D clippy::panic \
# -D clippy::panic_in_result_fn \
# -D clippy::print_stdout \
# -D clippy::print_stderr \
# -D clippy::dbg_macro \
# -D clippy::indexing_slicing \
# -D clippy::nursery \
# -D clippy::pedantic \
# -D clippy::allow_attributes \
# -D clippy::unwrap_used \
# -D clippy::expect_used \
# -D clippy::await_holding_lock \
# -D clippy::large_futures \
# -D clippy::todo \
# -A clippy::struct_field_names \
# -A clippy::cast_precision_loss \
# -A clippy::unused_self \
# -A clippy::future_not_send \

update-rust-deps:
    cargo upgrade
    cargo update

update-rust-deps-full:
    cargo upgrade --incompatible
    cargo update

test:
    cargo nextest run

clean:
    cargo clean

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

check: format lint test
