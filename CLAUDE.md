# Working in this repository

A Rust workspace for running Ascent Datalog at runtime and building
propose-then-verify loops on top of it. Read [`docs/`](docs/) (the design records)
before any design change.

## The crates

- **`ascent-jit`** — a runtime interpreter for Ascent programs supplied as data.
  The relational core is an interpreter (a tree-walk of a relational IR with a
  stratified, semi-naïve fixed-point loop); `if`/`let`/head expressions are
  lowered to WebAssembly and run through a swappable `WasmExecutor`. The default
  executor (`WasmtimeExecutor`, behind the on-by-default `wasmtime` feature)
  JIT-compiles under `wasmtime`; encoding is shared and runtime-free, so the
  crate also builds `--no-default-features` for `wasm32`. Provides query,
  provenance (`explain`), and speculative (`fork / assert / run / discard`)
  evaluation. See [docs/0004](docs/0004-pluggable-wasm-execution.md).
- **`fact-query`** — a governance-free proposer/verifier substrate over an
  `ascent-jit` fact base. The `FactStore` trait is the query seam; `FactSource`
  is the produce seam. v1 ships the conjunctive **queries grain** only.
- **`ascent-jit-web`** — a `wasm32`-only, workspace-*excluded* crate (like
  `ascent-jit/fuzz`): a `WasmExecutor` over the browser's `WebAssembly` engine so
  queries evaluate in place in the page. Built separately:
  `cargo build -p ascent-jit-web --target wasm32-unknown-unknown`.
- **`fact-query-node`** — a `wasm32`-only, workspace-*excluded* crate: the
  Node.js/TypeScript binding. wasm-bindgen-exports a `FactEngine` over
  `fact-query` (reusing `ascent-jit-web`'s `WebExecutor`), published as an npm
  package with a hand-written TS wrapper. Built via its own npm pipeline
  (`just node-build` / `cd fact-query-node && npm run build`), not `cargo`. See
  [docs/0006](docs/0006-typescript-node-binding.md).

`fact-query` and `ascent-jit-web` depend on `ascent-jit`; `fact-query-node`
depends on `fact-query` and `ascent-jit-web`. No crate depends on any
application.

## Invariants that are easy to violate

- **Governance-free / dependency direction.** `fact-query` (and `ascent-jit`)
  must never learn an application's policy vocabulary — denial relations, trust,
  origin, commit. Policy layers *on top* and must not leak *down*. The crate
  boundary, enforced by the dependency direction, is the guarantee. Reject any
  change that names an application concept in these crates.
- **Provenance is the engine's; trust is the app's.** *Which facts derived a
  tuple* is an engine concern and stays here. *Trust/origin semantics over
  provenance* is an application concern, expressed as a lattice in user rules —
  never an engine builtin.
- **Fail closed.** An indeterminate evaluation (e.g. the iteration bound is hit)
  is an error, never silently "nothing derived" / "no violations." A
  safety-conscious caller must treat it as a denial.
- **The value model is closed and `Copy`.** `Value` is `{Int(i64) | Bool | Sym}`.
  Hashing, the WASM bit-bridge, and the differential oracle all rest on this.
  Don't widen it; lower rich external data to these three kinds (see
  [docs/0003](docs/0003-external-fact-sources.md)).
- **The query contract checks form, not meaning.** `eval` returns a `ResultSet`,
  never an `Answer`. Do not let the API imply intent-fidelity it does not
  guarantee. See [docs/0002](docs/0002-fact-query-substrate.md).
- **Bound every evaluation.** Conjunctive query evaluation is capped on
  *cardinality* (not time); a forgotten cap is a memory-exhaustion DoS. Hitting
  the cap is a first-class, surfaced outcome, not an error.

## Development

- The **`Justfile` is the source of truth** for how the workspace is verified;
  CI shells out to the same recipes. Run the full suite before pushing:
  ```sh
  just check          # fmt + clippy + test (--all-features) + doc
  ```
  Individual recipes: `just fmt`, `just clippy`, `just test`, `just doc`,
  `just fuzz-quick [ms]`, `just fuzz <target> [seconds]`, `just build`.
- Use `--all-features` when testing: the differential-fuzz suite
  (`tests/fuzz_diff.rs`) is gated behind the `arbitrary` feature and is a no-op
  without it.
- `just doc` builds docs with `RUSTDOCFLAGS="-D warnings"`, so broken intra-doc
  links (including links from public items to private ones) fail the build.
- **Lints are strict and deny-by-default** (see the workspace `Cargo.toml`):
  `warnings`, `missing_docs`, `unsafe_code`, and clippy `all` + `pedantic` are all
  `deny`. Every public item needs a doc-comment; `unsafe` is forbidden;
  `#[allow(...)]` is denied (`allow_attributes`). Write code that passes clean.
- The `fact-query` crate README is compiled as a doctest (via `include_str!`), so
  keep its code snippet honest against the public API.
- The fuzz crate (`ascent-jit/fuzz`) is excluded from the stable workspace and run
  separately with nightly + `cargo-fuzz`:
  ```sh
  cd ascent-jit/fuzz && cargo +nightly fuzz run <target>
  ```
  Its `arbitrary`-based generators live in `ascent_jit::fuzz` (behind the
  `arbitrary` feature) and are shared with the in-tree `arbtest` tests.

## Version control

This repo is managed with [`jj`](https://github.com/jj-vcs/jj) (Jujutsu),
colocated with git. Use `jj` for commit/stack operations.
