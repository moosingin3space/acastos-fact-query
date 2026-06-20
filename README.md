# ascent-jit & fact-query

Two composable Rust crates for running [Ascent](https://github.com/s-arash/ascent)
Datalog **at runtime** and building **propose-then-verify** loops on top of it.

- **[`ascent-jit`](ascent-jit/)** — a runtime interpreter for Ascent programs
  supplied *as data*. Load a program from text or IR, assert facts, run to a fixed
  point, query relations, and ask *why* a tuple was derived. Rule expressions are
  lowered to WebAssembly and JIT-compiled under a fuel-metered, import-free
  sandbox, so a rich expression language stays safe even when the rules come from
  an untrusted source.
- **[`fact-query`](fact-query/)** — a **governance-free** proposer/verifier
  substrate over an `ascent-jit` fact base. It parses, form-checks, and evaluates
  conjunctive queries read-only under a cardinality bound, returning results *with
  provenance*. It carries no policy of its own — no LLM, no commit path, no denial
  vocabulary — so applications layer their own governance on top without it leaking
  down.

## Why

Ascent is a compile-time macro: the rules are frozen at `rustc` time. That is the
wrong shape when the rules themselves change at runtime — when some upstream
process (a person, a generative model, a configuration source) adds, edits, and
retracts rules and facts during execution and the engine must reason over the new
ruleset immediately.

`ascent-jit` makes the program runtime data. `fact-query` builds on the engine's
speculative evaluation and provenance to offer the recurring neurosymbolic
primitive once, honestly: **an untrusted artifact is proposed, the deterministic
engine evaluates it under a resource bound and reports what it does (with
provenance), and some net — a human, a vote, or nothing — decides whether that is
what was wanted.** The engine, not the proposer, is the authority.

## Quickstart

```rust
use ascent_jit::{Engine, Value};

// Load a program *as data*, assert facts, run to a fixed point.
let mut engine = Engine::from_source(
    "relation edge(int, int);
     relation path(int, int);
     path(x, y) <-- edge(x, y);
     path(x, z) <-- edge(x, y), path(y, z);",
)
.unwrap();
engine.add_fact("edge", vec![Value::Int(1), Value::Int(2)]).unwrap();
engine.add_fact("edge", vec![Value::Int(2), Value::Int(3)]).unwrap();
engine.run().unwrap();
assert_eq!(engine.query("path").len(), 3);
```

Layering `fact-query` on top adds parse → form-check → bounded read-only
evaluation with provenance; see [`fact-query/README.md`](fact-query/README.md) for
the contract and an example.

## Design

The reasoning behind these crates lives in [`docs/`](docs/):

1. [A runtime interpreter for Ascent](docs/0001-ascent-jit-runtime-engine.md)
2. [The `fact-query` proposer/verifier substrate](docs/0002-fact-query-substrate.md)
3. [A pluggable external fact-source seam](docs/0003-external-fact-sources.md)

## Building

```sh
cargo build
cargo test
cargo clippy --all-targets
```

The fuzz targets under [`ascent-jit/fuzz`](ascent-jit/fuzz/) are excluded from the
stable workspace and run separately with a nightly toolchain:

```sh
cargo +nightly fuzz run program        # in ascent-jit/fuzz
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
