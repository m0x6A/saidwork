# Developer shortcuts. Install `just` with `cargo install just` or `dnf install just`.

default: check

# Format, lint and test everything.
check: fmt-check clippy test

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
    cargo test --workspace --all-features

run *ARGS:
    cargo run -p saidwork -- {{ARGS}}
