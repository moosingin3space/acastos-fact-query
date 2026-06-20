# 0004: A pluggable WebAssembly execution tier

- **Status:** Proposed
- **Crate:** `ascent-jit` (with a `wasm32` consumer crate, `ascent-jit-web`)
- **Builds on:** [0001](0001-ascent-jit-runtime-engine.md) (the WebAssembly
  expression tier)

## Context

`ascent-jit` lowers each `if`/`let`/head expression to a tiny, import-free
WebAssembly module exporting one function `f(i64..) -> i64`, then runs it (ADR
0001). Until now the *running* was welded to **`wasmtime`**: `WasmEval` owned a
`wasmtime::Engine` and `Store` and instantiated and called the module inline.

That weld blocks an attractive deployment: compiling the whole stack to `wasm32`
and evaluating fact queries **in the browser, in place** — a page that loads a
fact base and runs `fact-query` over it with no server round-trip. `wasmtime` is
a *native* runtime; it does not run inside a browser-wasm context. But the
browser already ships a perfectly good WebAssembly engine. The thing we need to
swap is small and well-isolated.

The key observation: the expression tier is already two separable jobs.

- **Encoding** — turning an `Expr` into module bytes (`encode_module`). This is
  pure, allocation-only, depends on nothing but `wasm-encoder`, and compiles to
  any target including `wasm32`.
- **Execution** — instantiating those bytes and calling `f`. This, and *only*
  this, was `wasmtime`-specific.

## Decision drivers

- **Run in place in the browser.** The motivating use case: evaluate queries on
  the client over the platform `WebAssembly` engine, no native runtime.
- **Keep `ascent-jit` dependency-clean.** The engine crate must not grow a
  `wasm-bindgen`/`js-sys` dependency, nor a browser-only build mode, to serve a
  consumer that wants in-browser execution.
- **One encoder, many runtimes.** The lowering is the hard, semantics-bearing
  part and must not fork per backend; only execution may differ.
- **Preserve the differential guarantee (0001).** The pure interpreter is the
  oracle. A new runtime must not be able to drift from it.
- **Fail closed, stay sandboxed (0001).** Swapping the runtime must not weaken
  the structural sandbox (import-free modules) or the bounded-evaluation contract.

## Decision

Factor execution behind a trait and make the native runtime an optional feature.

1. **`WasmExecutor` seam.** A trait with an associated `Module` type and two
   methods — `instantiate(bytes) -> Module` and `call(&Module, &[i64]) -> i64` —
   captures exactly the "run these bytes" job. `WasmEval` becomes generic over a
   `WasmExecutor`, caching one compiled module per expression shape as before;
   the encoding path is untouched and shared.

2. **`WasmtimeExecutor`, behind a default feature.** The old inline `wasmtime`
   code moves verbatim into the trait impl, gated by a `wasmtime` Cargo feature
   that is **on by default**. `ascent-jit` with `--no-default-features` drops
   `wasmtime` entirely and still compiles (encoder + pure interpreter + the
   generic `WasmEval`), so a `wasm32` build is possible. `fact-query` forwards
   the same feature.

3. **The plug point.** `Engine::from_source_with_evaluator(src, Box<dyn
   ExprEval>)` is public, so a host injects `WasmEval::with_executor(my_executor)`
   without `ascent-jit` knowing the executor exists.

4. **The browser backend lives in its own crate.** `ascent-jit-web` (a `wasm32`,
   workspace-*excluded* crate, mirroring how `ascent-jit/fuzz` detaches) provides
   `WebExecutor`, a `WasmExecutor` over synchronous
   `WebAssembly.Module`/`Instance`, plus an `engine_from_source` helper. It
   depends on `ascent-jit` with `default-features = false`. This keeps the
   browser-only dependencies — and the inability to run in native CI — out of the
   engine crate.

## Why fuel is a `wasmtime` detail, not a contract

`wasmtime` execution is fuel-metered: ADR 0001 cites it as the guard against an
expression wedging the fixed-point loop. The browser engine offers no fuel, so it
is fair to ask whether dropping it weakens safety. It does not, for *these*
modules: `encode_module`/`emit` produce only straight-line arithmetic and
`if`/`else` blocks — **no loops, no calls, no back-edges** — so every emitted
module terminates structurally. Fuel was a defense-in-depth backstop on a runtime
that *could* in principle run unbounded code, not a property the expression tier
relies on. A fuel-less executor is therefore safe for the modules this engine
emits. (Were the encoder ever to emit loops, this reasoning — and the seam's
contract — would have to be revisited.)

## Consequences

- **The differential oracle still pins every backend.** Executors differ only in
  how they run *identical* bytes; the pure interpreter remains the oracle, and the
  existing interp-vs-wasm differential/fuzz suite continues to guarantee the
  native path. The browser path executes the same bytes by construction, so it
  inherits the same semantics — though it cannot be exercised by the native suite
  and is verified by compilation against the seam.
- **No behavior change on native.** `WasmtimeExecutor` is the moved-verbatim old
  code; default builds and all existing tests are unaffected.
- **The value model stays closed (0001/0002).** The `i64` bit-bridge is unchanged;
  the browser executor marshals each `i64` across the JS boundary as a `BigInt`
  (the function's `i64` signature triggers WebAssembly BigInt-integration) and
  re-tags the result host-side, exactly as the native path does.
- **Governance-free boundary intact (0002).** The seam is purely mechanical —
  bytes in, `i64` out. No application vocabulary enters either crate.
- **Cost.** One more public trait and a feature flag in `ascent-jit`; a small
  `wasm32`-only crate that native CI cannot run. The browser backend's
  correctness rests on the shared-bytes argument above, not on its own test pass.
