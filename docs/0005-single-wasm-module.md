# 0005: One WASM module, many entrypoints

- **Status:** Proposed
- **Crate:** `ascent-jit` (with the `wasm32` consumer crate, `ascent-jit-web`)
- **Builds on / amends:** [0004](0004-pluggable-wasm-execution.md) (the
  `WasmExecutor` seam). This record changes the seam's `call` signature and the
  shape of what `encode_module` emits; everything else in 0004 stands.

## Context

ADR 0001/0004 lower each `if`/`let`/head expression to its **own** tiny,
import-free WebAssembly module exporting a single function `f(i64..) -> i64`,
and `WasmEval` instantiates one such module per distinct expression shape,
lazily, on first evaluation. A program therefore produces *N* modules and *N*
instantiations.

That is fine under `wasmtime` but awkward in the browser. `ascent-jit-web`'s
`WebExecutor` pays a `WebAssembly.Module` + `WebAssembly.Instance` round-trip
across the JS/wasm boundary **per expression shape**, and the host has *N* live
module/instance objects to manage rather than one. The motivating deployment of
0004 — evaluate a fact base in place, in the page — wants the engine to hand the
browser **one** artifact to compile and instantiate.

## The load-bearing observation

The bytes `encode_module`/`emit` produce for an expression depend only on the
expression's structure and the *order* of its free variables
(`Expr::collect_vars`). They do **not** depend on the variables' types: `emit`
reads only param positions and `Value::to_bits()` for literals. Types enter the
old cache key purely to **re-tag the `i64` result** host-side
(`Value::from_bits(bits, ty)`) — that is metadata, not code.

So two evaluations of the same `Expr` whose free variables have different types
compile to *identical* functions. The unit of compilation is the `Expr`; the
type only chooses how the returned `i64` is reinterpreted.

This means a single module can carry **one function per distinct `Expr`**
(exported `f0`, `f1`, …) with no per-type duplication, and **no static
type-inference pass** is required to build it — the result type stays a cheap,
lazily-computed side-table keyed on `(Expr, free-var types)`, exactly the
information the old key already carried.

## Decision

1. **One module, function-per-`Expr`.** `encode_module` takes the set of
   distinct expressions (each with its `collect_vars` ordering) and emits a
   single module exporting `f0..f{n-1}`, one function per expression. `WasmEval`
   holds **one** instantiated module and a map `Expr -> index`.

2. **Eager-prime, then lazy-extend.** A new `ExprEval::prime(&[&Expr])` (default
   no-op; implemented by `WasmEval`) lets the `Engine` register *all* of a
   program's expressions at construction, so the module is built and instantiated
   **once** up front and a `run()` instantiates nothing. Expressions that only
   appear in ad-hoc queries (`evaluate_query`/`explain`) are not known then; the
   first time `eval_expr` sees an unregistered `Expr` it is appended and the
   single module is re-encoded and re-instantiated. Steady state is one live
   module; warm-up cost is bounded by the number of *distinct* expressions, not
   by evaluations.

3. **The seam gains a function selector.** `WasmExecutor::call` becomes
   `call(&Module, func: &str, args: &[i64])`; `instantiate` now returns a handle
   to the whole instance rather than a single function. `WasmEval` owns the
   `f{i}` naming, so the executor stays purely mechanical — bytes in, instance
   out, `(name, args)` in, `i64` out. `WasmtimeExecutor::Module` becomes
   `wasmtime::Instance` (resolving the func by name per call, still
   fuel-metered); the browser `WebExecutor` likewise holds the instance and
   resolves `f{i}` from its exports.

## Why this keeps every 0001/0004 invariant

- **The differential oracle still pins every backend.** The *bytes of each
  function* are byte-for-byte what the old per-expression encoder produced — the
  functions were merely collected into one module. The pure interpreter remains
  the oracle and the existing interp-vs-wasm differential/fuzz suite continues to
  exercise the native path unchanged (it drives `WasmEval::eval_expr`, which now
  transparently builds a one- or many-function module behind the same API).
- **Still straight-line, still fuel-optional (0004).** Collecting functions into
  one module adds no loops, calls between functions, or back-edges; each `f{i}`
  is the same arithmetic-plus-`if`/`else` body as before, so termination is still
  structural and fuel remains a `wasmtime` backstop, not a contract.
- **The value model stays closed (0001/0002).** The `i64` bit-bridge is
  untouched; result re-tagging by inferred `Type` is exactly as before, just
  cached as metadata beside the `Expr -> index` map.
- **Governance-free boundary intact (0002).** `prime` and the new `call`
  signature are mechanical (`&[&Expr]` in, instance/`i64` out). No application
  vocabulary enters either crate.

## Consequences

- **Browser host gets one artifact.** After `engine_from_source`, the whole
  program's expression tier is a single compiled/instantiated module; `run()` and
  query evaluation call into it by name with no further instantiation.
- **Compilation moves to construction.** `from_source*` now compiles the
  expression module eagerly (via `prime`) instead of on first `run()`. For a
  program that is built but never run this is a small added cost; for the common
  path it is the same work, paid once and earlier.
- **Ad-hoc queries can still trigger a rebuild.** An ad-hoc query introducing an
  expression absent from the program re-instantiates the single module once. This
  is the price of supporting expressions not known at load time; it is bounded by
  the number of distinct such expressions, and could later be avoided by
  pre-registering a query's parts before evaluation if it ever matters.
- **Cost.** One more (defaulted) trait method, a slightly richer `call`
  signature, and `WasmEval` state that grows from a per-shape cache to a single
  module plus an index. The encoder is marginally larger (a section loop instead
  of a single function); the per-function emission is unchanged.
