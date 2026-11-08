# Design records

These documents record the design decisions behind the two crates in this
workspace and the reasoning that led to them. They are written to be read in
order, but each stands on its own.

| # | Title | What it covers |
|---|-------|----------------|
| [0001](0001-ascent-jit-runtime-engine.md) | A runtime interpreter for Ascent | Why the engine runs Datalog supplied *as data*, the interpreter + WebAssembly expression tier, provenance, the bounded fixed point, and speculative evaluation. |
| [0002](0002-fact-query-substrate.md) | The `fact-query` proposer/verifier substrate | The recurring propose → speculate → (delta, provenance) → net primitive, the governance-free boundary, the queries grain, and the five-guarantee contract. |
| [0003](0003-external-fact-sources.md) | A pluggable external fact-source seam | The produce-side `FactSource` seam, the schema contract (column types + drift detection), lowering rich external tuples into the closed value model, and content-addressed identity. |
| [0004](0004-pluggable-wasm-execution.md) | A pluggable WebAssembly execution tier | Splitting the expression tier into shared *encoding* and a swappable `WasmExecutor`, making `wasmtime` an optional default feature, and the `wasm32` `ascent-jit-web` crate that runs queries in the browser in place. Why fuel is a `wasmtime` detail, not a contract. |
| [0005](0005-single-wasm-module.md) | One WASM module, many entrypoints | Collecting every expression into a single module exporting `f0..fN` (one function per `Expr`, since the emitted bytes are type-independent), eager-priming it at construction with lazy extension for ad-hoc queries, and the resulting `WasmExecutor::call(func, ..)` seam change — so the browser host gets one artifact to instantiate. |
| [0006](0006-typescript-node-binding.md) | The TypeScript/Node.js binding | wasm-bindgen-exporting a `FactEngine` over `fact-query` (reusing `ascent-jit-web`'s `WebExecutor`), published as an npm package with a hand-written TS wrapper, built via its own npm pipeline rather than `cargo`. |
| [0007](0007-engine-snapshot-serialization.md) | Serializing an engine to a re-materializable snapshot | Persisting the *logical* state — IR + interner + materialized database — into a framed, versioned, content-addressed binary that re-materializes without re-parsing or re-running the fixed point, *regenerating* the wasm from the IR rather than storing it. Why precompiled native code is out of scope, and the trusted/verified load split. |
| [0008](0008-benchmarks-and-the-join-jit-deferral.md) | Measure first: the join-JIT deferral | The in-tree benchmarks (`just bench`, `npm run bench`) and what they showed: the naïve relational core (~n³·⁷ vs the macro's ~n²) dominates, the expression boundary vanishes on join-heavy work, and the nested wasm executor loses to the in-substrate interpreter on `wasm32`. Join JIT deferred (algorithm before codegen, native-first if ever); `FactEngine.fromSource` gains an evaluator choice. |
| [0009](0009-semi-naive-indexed-evaluation.md) | Semi-naïve, indexed, incremental evaluation | Replacing the naïve core: append-only row logs so deltas are row ranges, semi-naïve windows, hash indexes on bound columns (transient for read-only calls), incremental re-runs from the last fixed point, and one shared traversal. Transitive closure drops from ≈ n³·⁷ to the macro's ≈ n²; the gap is now a ~11× constant and the interpreter beats wasmtime on joins. |
| [0010](0010-retire-the-wasm-expression-tier.md) | Retire the WebAssembly expression tier | Making the interpreter the only expression backend: removing `ExprEval`, the wasm encoder/executors, `wasmtime`, and `ascent-jit-web`, and the Node binding's evaluator choice. The wasm tier lost on every measured workload and its sandbox guarded a total expression language; oracles are retargeted to type-soundness and semi-naïve-vs-naïve. Supersedes 0004/0005. |
| [0011](0011-cloudflare-workers-build.md) | A Cloudflare Workers build of the npm package | One package, two builds: a `workerd` export condition selects an ESM build over wasm-bindgen's `--target web` glue, instantiated with `initSync` from the Workers-imported module (no patching, no runtime compilation). A shared TS core with an injected binding, and a Miniflare test that runs the build inside workerd. |
| [0012](0012-compiled-bodies-and-flat-storage.md) | Compiled rule bodies and flat relation storage | Static binding analysis compiles bodies to dense variable slots, per-column match ops, and expression bytecode (the tree interpreter becomes its oracle); `let` shadows per Ascent scoping. Relations become one flat row log + liveness bit set + seeded open-addressing key tables for dedup, lattice keys, and indexes. Transitive closure −73% (3.1× the macro, from 11×), queries −63%. |

## Conventions

- **Status** is one of *Proposed*, *Accepted*, or *Superseded*.
- A record is a snapshot of the reasoning at a point in time. Later records may
  build on or revise earlier ones; they say so explicitly when they do.
- The records describe *why*, not *how* — the code and its doc-comments are the
  authority on the current API.
