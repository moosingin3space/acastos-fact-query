# 0010: Retire the WebAssembly expression tier

- **Status:** Accepted
- **Crates:** `ascent-jit`, `fact-query`, `fact-query-node`; removes
  `ascent-jit-web`
- **Supersedes:** [0004](0004-pluggable-wasm-execution.md) and
  [0005](0005-single-wasm-module.md) in full; the expression-sandbox decision
  (item 6) of [0001](0001-ascent-jit-runtime-engine.md); the evaluator choice
  (decision 4) of [0008](0008-benchmarks-and-the-join-jit-deferral.md); and the
  `WebExecutor` reuse in [0006](0006-typescript-node-binding.md)
- **Builds on:** [0009](0009-semi-naive-indexed-evaluation.md)

## Context

ADR 0001 lowered `if`/`let`/head expressions to WebAssembly and ran them under
`wasmtime`. The reason given was a sandbox: fuel metering and no host imports
would let the engine "offer a *rich* expression language safely instead of a
hand-built total one". A pure tree-walk interpreter of the same `Expr` IR was
kept as the differential oracle. Later records built more on the tier: an
executor seam ([0004](0004-pluggable-wasm-execution.md)), a browser executor
crate (`ascent-jit-web`), a single shared module
([0005](0005-single-wasm-module.md)), and the Node binding's nested executor
([0006](0006-typescript-node-binding.md)).

The measurements since then point one way:

- **0008:** a single expression call costs ~1.5 µs under wasmtime vs ~100 ns
  interpreted, and an expression-heavy program runs 6.5× faster interpreted.
  Under Node, the in-substrate interpreter beat the nested `WebExecutor` on
  every workload (up to ~13×).
- **0009:** with the relational core no longer naïve, join bookkeeping stopped
  hiding the boundary. The interpreter beats wasmtime by about 2× on join-heavy
  fixed points and on conjunctive queries too.

No workload measured favours the wasm tier. The sandbox argument also never
became concrete. The expression language *is* the hand-built total one: `Expr`
has literals, variables, and unary and binary operators, with no loops, calls,
recursion, or host access. Both backends evaluated that same IR, so fuel and
the import-free module protected against failure modes the language cannot
express. ADR 0004 already found that fuel was a `wasmtime` detail rather than
a contract, because the browser engine has none.

## Decision

1. **The interpreter is the only expression backend.** `Expr::eval` is called
   directly by the relational core. The `ExprEval` trait, `Interpreter`,
   `WasmEval`, the `WasmExecutor` seam, `WasmtimeExecutor`, the `wasm` module,
   `Program::exprs` (which existed only to prime the module), and the `i64`
   value bit-bridge (`Value::to_bits` / `from_bits`) are removed.
   `ExprError::Runtime` (wasm traps and fuel) and `Error::Wasm` go with them.
2. **One constructor.** `Engine::from_source` builds an interpreter-backed
   engine; `from_source_interpreted` and `from_source_with_evaluator` are gone.
   `Engine::explain` and `Engine::evaluate_query` now take `&self`, because
   there is no evaluator state left to mutate.
3. **Dependencies and features.** `ascent-jit` drops `wasmtime` and
   `wasm-encoder` and has no default features. `fact-query` drops its forwarding
   `wasmtime` feature. Every crate now builds for `wasm32` as-is.
4. **`ascent-jit-web` is deleted.** Its only job was a browser `WasmExecutor`.
   A browser host now compiles `ascent-jit` (or `fact-query`) straight into its
   own wasm module.
5. **The Node binding loses its evaluator choice.**
   `FactEngine.fromSource(src)` takes no second argument, and the `Evaluator` TS
   type is removed. A JS caller that still passes one has it ignored; a
   TypeScript caller gets a type error. `npm run bench` now reports the
   interpreter alone.
6. **The oracles are retargeted, not dropped.** The wasm-vs-interpreter
   expression property becomes `fuzz::expr_soundness`: on random well-typed
   expressions, the interpreter yields a value of the inferred type or an
   arithmetic fault, never a panic, unbound variable, or type mismatch. The
   wasm-vs-interpreter program property is replaced by 0009's
   `differential_seminaive`. The `cargo-fuzz` targets `expr` and `program`
   drive these two. The macro differentials are unchanged.

## Why not keep it optional

A disabled-by-default tier would still cost a trait object on every
expression, a second semantics to keep pinned, a feature matrix in CI, and an
executor seam whose only browser implementation loses to not using it. If a
future expression language gains loops, calls, or user functions, that is the
moment to revisit sandboxing. It would be a design question about *that*
language, answered by a new record, not a reason to keep this tier warm.

## Consequences

- **Smaller and simpler.** Roughly a thousand lines of encoder, executor, and
  plumbing are deleted, and the dependency graph loses `wasmtime` and its
  Cranelift build. `ascent-jit` builds for `wasm32` with no feature juggling.
- **Faster on every measured workload.** This rests on the 0008/0009 numbers;
  nothing in the interpreter path changed.
- **Safety rests on totality.** Expressions cannot loop or call out because the
  IR cannot express it, and arithmetic faults (division by zero, overflow) are
  `ExprError::Arithmetic`, as before. `unsafe` stays forbidden workspace-wide.
- **Recursion is bounded, and deep input fails closed.** With the sandbox gone,
  the remaining failure mode was the stack. Parsing, `infer`, `eval`, and the
  body walk all recurse; the wasm encoder had recursed the same way.
  - **Depth limit.** `expr::MAX_EXPR_DEPTH` (256) bounds expression *tree*
    depth. The parser tracks each subtree's depth as it builds, so a flat chain
    like `1 + 1 + …`, which parses in a loop, is rejected before a too-deep tree
    exists. Dropping such a tree would itself recurse. Parentheses, `Dual(..)`,
    and prefix-operator runs are bounded separately. On a hand-built tree,
    `eval`/`infer` return `ExprError::DepthLimit`, and `Expr::depth` measures
    depth without recursing.
  - **Stack growth.** A rule body's clause count is not bounded, so the body
    walk, the parser's nesting, and the expression walks run under
    `stacker::maybe_grow` on native targets (`stack.rs`). Growth is
    native-only: on `wasm32`, `stacker` cannot find the stack limit and would
    allocate a segment per call. There, the depth limit is the defence, and an
    overflow is a WebAssembly trap rather than memory corruption.
  - **Cost.** About 2–3% on the join, query, and expression benchmarks.
  - **Tests.** `tests/limits.rs` covers each shape, the exact boundary, and a
    100 000-clause body; the body test overflows without `stacker`.
  - **Still recursive.** The derived `Drop`, `Clone`, `Debug`, and `PartialEq`
    of `Expr`. Parsed trees are bounded; a caller hand-building a pathologically
    deep tree owns that risk. `fact-query`'s safety check is now iterative.
- **The crate name is now historical.** `ascent-jit` no longer JIT-compiles
  anything. Renaming is a separate, breaking decision and is not made here.
- **Breaking API.** This changes the public surface of `ascent-jit`,
  `fact-query`'s features, and the npm package's `fromSource` signature. All are
  pre-1.0 (`0.1.0`).
