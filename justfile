test:
    agent-slot -- cargo test --locked

lint:
    cargo fmt -- --check
    agent-slot -- cargo clippy --locked --all-targets --all-features -- -D warnings
