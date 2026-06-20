# Justfile — single source of truth for build, lint, test, and docs.
# CI shells out to these recipes so there is exactly one definition of
# "how this workspace is built and verified".

# Default to running the full verification suite.
default: check

# Verify formatting.
fmt:
    cargo fmt --all -- --check

# Run clippy across the whole workspace.
clippy:
    cargo clippy --workspace --all-targets --all-features

# Run the test suite (all features, so the arbtest differential-fuzz suite runs).
test:
    cargo test --workspace --all-features

# Build the public API docs, denying warnings (broken intra-doc links, etc.).
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps

# Full workspace check: formatting, clippy, tests, and docs.
check: fmt clippy test doc

# Differential fuzzing via the in-tree arbtest suite (stable toolchain). These
# also run under `check` at arbtest's default budget; this recipe lets you crank
# the budget up. Usage: `just fuzz-quick 30000` for 30s per property.
fuzz-quick ms="5000":
    ARBTEST_BUDGET_MS={{ms}} cargo test -p ascent-jit --features arbitrary --test fuzz_diff

# Coverage-guided differential fuzzing via cargo-fuzz (needs nightly +
# `cargo install cargo-fuzz`). Targets: expr, program, macro_transitive_closure,
# macro_even_odd, macro_shortest_path. Usage: `just fuzz program 300`.
fuzz target="program" seconds="60":
    cd ascent-jit && cargo +nightly fuzz run {{target}} -- -max_total_time={{seconds}}

# Build the release artifacts for the whole workspace.
build:
    cargo build --release --workspace
