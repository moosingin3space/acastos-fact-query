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

# Run the `ascent-jit` criterion benchmark suite (joins, expressions, queries).
bench:
    cargo bench -p ascent-jit

# Save the benchmarks as a named criterion baseline (criterion targets only: the lib harness rejects these flags).
bench-save name:
    cargo bench -p ascent-jit --bench joins --bench expr --bench queries -- --save-baseline {{name}}

# Compare the benchmarks against a saved criterion baseline (criterion targets only: same reason as bench-save).
bench-compare name:
    cargo bench -p ascent-jit --bench joins --bench expr --bench queries -- --baseline {{name}}

# Build the release artifacts for the whole workspace.
build:
    cargo build --release --workspace

# Build the Node.js/TypeScript binding (`fact-query-node`): wasm32 crate ->
# wasm-bindgen glue -> tsc. Needs the wasm32 target and a matching
# `wasm-bindgen-cli` (see the crate README). Detached from the workspace, so it
# is not part of `just check`.
node-build:
    cd fact-query-node && npm install && npm run build

# Build the Node.js binding and run its test suite.
node-test:
    cd fact-query-node && npm install && npm test
