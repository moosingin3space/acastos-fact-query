# 0008: Measure first — the join-JIT deferral and the expression-backend choice

- **Status:** Accepted
- **Crates:** `ascent-jit` (benchmarks), `fact-query-node` (evaluator selection)
- **Builds on:** [0001](0001-ascent-jit-runtime-engine.md) (the deferred
  whole-rule-body JIT), [0004](0004-pluggable-wasm-execution.md) /
  [0005](0005-single-wasm-module.md) (the executor seam),
  [0006](0006-typescript-node-binding.md) (the Node binding and its
  wasm-in-wasm cost)

## Context

ADR 0001 deferred compiling whole rule bodies — joins included — to WASM as "a
hard, unproven win," citing the per-crossing cost of the host↔WASM boundary. A
proposal to take that tier up now, motivated by that same boundary cost — and
especially by the Node/browser deployment, where the substrate itself is wasm
and every expression call trampolines substrate-wasm → JS (`BigInt` boxing,
`Reflect.get`, `Function.apply`) → inner-wasm and back — turned on assumptions
nobody had measured:

1. how much of evaluation time the expression boundary actually costs, native
   and nested;
2. how the relational core — currently *naïve*, not semi-naïve: no deltas, no
   indexes, and a whole-relation clone per body-atom visit
   (`Database::snapshot`) — scales against the compiled `ascent!` macro;
3. whether the nested `WebExecutor` in the wasm32 deployment even beats the
   in-substrate pure interpreter it is used instead of.

Rather than argue, we measured. The evidence lives in the workspace: a
criterion suite in `ascent-jit/benches/` (`just bench`) and a Node suite in
`fact-query-node/bench/` (`npm run bench`).

## What the benchmarks say

Machine caveat up front: one loaded laptop (Intel i5-8350U, Linux). The
scaling exponents and cross-backend *ratios* were stable across runs; treat
absolute times as order-of-magnitude.

**1. The gap to the macro is algorithmic, not the dynamic-typing tax.**
Transitive closure over a chain of *n* edges, `run()` only, medians:

| n  | jit (wasmtime) | jit (interpreted) | `ascent!` macro |
|----|---------------:|------------------:|----------------:|
| 8  | 628 µs         | 238 µs            | 13.0 µs         |
| 16 | 5.39 ms        | 1.97 ms           | 26.6 µs         |
| 32 | 49.1 ms        | 34.7 ms           | 131 µs          |
| 64 | 620 ms         | 513 ms            | 567 µs          |

A log-log fit gives the naïve core **≈ n³·⁷** against the macro's **≈ n²·¹**
(essentially the size of the answer — a chain's closure has n(n+1)/2 paths).
Because the gap is an *exponent*, it grows: 18–49× at n=8, **~900–1100× at
n=64**. ADR 0001's "plausibly several-fold" dynamic-typing tax is only the
small-n story; the dominant cost is the missing semi-naïve/index machinery,
which no constant-factor backend change can recover.

**2. The expression boundary vanishes exactly where a join JIT would live.**
Native, one `eval_expr` call costs ~1.5 µs under wasmtime vs ~100 ns
interpreted (~15×), and an expression-heavy program runs 6.5× faster
interpreted. But on the queries grain — a 3-atom join with one trivial
condition over a few hundred tuples — the two backends are statistically
indistinguishable (7.2 ms vs 8.1 ms): join bookkeeping drowns the expression
tier. The boundary overhead a join JIT would eliminate is already ~1× on
join-dominated work; what a join JIT could actually buy is specialization,
and that case cannot be made while the algorithm above it is naïve.

**3. In the wasm32 deployment the nested executor loses everywhere.** Under
Node (v26), the in-substrate pure interpreter beat the nested `WebExecutor` on
every workload — ~13× on expression-heavy `run()`, 3–5× on transitive closure
(pure joins: even head-tuple construction crosses the boundary per
derivation), ~1.6× on `query()`. This is ADR 0006's predicted "wasm-in-wasm
cost," quantified. The trampoline is real, and the winning fix is *removing*
the inner runtime in that deployment, not enlarging it — the sandbox
motivation for the WASM tier (0001) does not apply there, because the whole
substrate already sits inside the host's WebAssembly sandbox, and the
interpreter is the differential oracle: identical semantics by construction.

## Decision

1. **The join JIT stays deferred — now on evidence.** Reaffirming 0001, with
   the reasons sharpened: (a) the dominant cost is the naïve core's exponent,
   which a JIT would faithfully preserve; (b) the boundary cost a join JIT
   would remove is already negligible on join-heavy work; (c) compiling joins
   would emit *looping* modules, crossing 0004's explicit revisit-trigger —
   structural termination is lost, fuel becomes a contract, and the browser
   engine has no fuel — and would require a data-plane ABI (relations in
   linear memory or per-tuple host callbacks) plus provenance bookkeeping in
   generated code. If the tier is ever revisited, it is native-first, where
   `wasmtime`'s fuel makes loops defensible.

2. **The next performance work is algorithmic**: semi-naïve evaluation with
   delta relations, indexes on bound columns, and eliminating the per-atom
   `snapshot` clones. That design gets its own record; this one only fixes the
   ordering — algorithm before codegen.

3. **The benchmarks are workspace citizens.** `just bench` and `npm run
   bench` stay in-tree so the assumptions this record rests on remain
   re-testable; a future core rewrite re-runs them rather than re-arguing.

4. **wasm32 hosts choose their expression backend.**
   `FactEngine.fromSource(src, evaluator)` accepts `"wasm"` (the default —
   today's contract-pinned path, unchanged) or `"interpreted"` (the pure
   interpreter, in-substrate, zero crossings). The two are semantically
   identical — the interpreter *is* the oracle the wasm bytes are pinned to —
   so the choice is purely mechanical and the 0006 contract (stage-tagged
   errors, no new grain, governance-free) is untouched. Keeping `"wasm"` as
   the default keeps this change additive; flipping the default is a live
   follow-up, on these numbers.

## Consequences

- **The honest number 0001 asked for exists.** Its risk section wanted the
  dynamic-typing tax benchmarked against the macro; the answer is that the tax
  is real but secondary — the naïve core is the liability, and it is now a
  *measured*, documented one until the semi-naïve record lands.
- **A workspace-hygiene fix rode along.** The root `[workspace.dependencies]`
  entry for `ascent-jit` now sets `default-features = false`: without it,
  feature unification forced `wasmtime` into every `wasm32` graph and broke
  the `fact-query-node` build outright. Native consumers get the JIT back
  through `fact-query`'s on-by-default `wasmtime` feature. The rule this
  encodes: workspace-level deps stay feature-minimal; consumers opt in.
- **Cost.** A criterion dev-dependency and three bench targets in
  `ascent-jit`; a `bench` recipe; an `evaluator` parameter and bench script in
  `fact-query-node`. No engine code changed.
- **Risk.** One machine, variable load. The conclusions lean on exponents and
  ratios, which were stable; anyone re-litigating the absolute numbers should
  re-run the suites first.
