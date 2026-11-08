# 0009: Semi-naïve, indexed, incremental evaluation

- **Status:** Accepted
- **Crates:** `ascent-jit` (the relational core)
- **Builds on:** [0001](0001-ascent-jit-runtime-engine.md) (the evaluator this
  replaces), [0008](0008-benchmarks-and-the-join-jit-deferral.md) (the
  measurements that ordered this work ahead of any join JIT)

## Context

ADR 0008 measured the relational core and found the gap to the `ascent!` macro
was an *exponent*, not a constant: transitive closure over an `n`-edge chain
scaled as ≈ n³·⁷ against the macro's ≈ n²·¹. The core was naïve: every
iteration re-joined every rule against the whole database, every body atom
scanned its whole relation, and every atom visit cloned that relation
(`Database::snapshot`). 0008 fixed the order of work as algorithm before
codegen. The aim of this record is a core whose complexity tracks the size of
the answer, so that the interpreter is enough on its own and compiling rules
is unnecessary.

## Decision

**1. Relations are append-only row logs.** A `RelationStore` keeps its tuples
in insertion order in a `Vec`, with a hash set (or, for a lattice, the key map)
for deduplication. Because rows are only ever appended, "the tuples added since
point *p*" is the row range `p..`. A semi-naïve delta is then a pair of
integers, not a separate relation. A lattice tuple superseded by a joined
value is *tombstoned* and the joined tuple appended, so every reader skips dead
rows and the log stays append-only.

**2. Semi-naïve evaluation over row windows.** Each stratum still iterates to a
fixed point. Iteration *t* works on the delta `lo..hi`, the rows appended during
iteration *t − 1*. A rule with positive atoms `a₁..aₖ` fires once for each atom
`aᵢ` whose relation grew. In that firing, `aᵢ` reads `lo..hi`, the atoms before
it read `0..lo`, and the atoms after it read `0..hi`. Each combination of body
tuples is therefore joined exactly once over the run. Rows appended during
iteration *t* are outside every window until *t + 1*. Head tuples are buffered
for the length of one rule's firing (the walk borrows the database immutably)
and are dropped immediately if they are already stored or already pending.

Negation and aggregation read lower strata, so they are not monotone inputs to
a delta. A rule whose negated or aggregated relation changed re-fires in full.
A rule with no positive atoms fires on the first iteration only, unless one of
those guard inputs changes.

**3. Hash indexes on bound columns.** Planning walks each body left to right
and records, for every atom, the columns already bound when it is reached:
literals, plus variables bound by earlier clauses. Variables bound earlier in
the same atom are excluded; `unify` still checks those. The atom then probes a
hash index keyed on exactly those columns instead of scanning. Posting lists
hold row ids in append order, so restricting a lookup to a delta window takes
two binary searches. `Database::new(program)` registers the indexes the rules
need, and inserts maintain them. Read-only entry points (`eval_query`,
`explain`) reuse a persistent index when one fits. Otherwise they build a
*transient* index for that call. This keeps them literally unable to mutate the
database, which is the queries-grain guarantee of
[0002](0002-fact-query-substrate.md).

**4. Incremental re-runs.** When `run` completes, each relation records how
many of its rows are closed under the rules. The next `run`, after more facts
are added or on a `speculate` fork, starts its windows at those marks instead of
at zero, so the work is proportional to the consequences of the new facts, not
to the database. This gives the same result as re-running from scratch under
v1's monotone, no-retraction semantics, and the differential oracle below checks
exactly that. `speculate` likewise reads its `Consequences` off the fork's
appended rows instead of diffing whole relations.

**5. One traversal.** `run`, `explain`, and `eval_query` used to be three
near-copies of the same nested-loop solver. They are now one `Walk` over
planned steps, with a small `Sink` per entry point: derive, match a target, or
collect up to the cap. Provenance support is carried as `(relation, row id)`
pairs and materialized only when a sink needs it.

## Evidence

The same machine and caveats as 0008. Transitive closure over an `n`-edge chain,
`run()` only, medians. The previous-core column was re-measured on the same
machine immediately before the change (`just bench-save naive`):

| n  | interpreted, before | interpreted, after | wasmtime, after | `ascent!` macro |
|----|--------------------:|-------------------:|----------------:|----------------:|
| 8  | 95.7 µs             | 42.2 µs            | 98.0 µs         | 5.0 µs          |
| 16 | 1.00 ms             | 153 µs             | 307 µs          | 15.5 µs         |
| 32 | 12.4 ms             | 580 µs             | 1.09 ms         | 52.7 µs         |
| 64 | 175 ms              | 2.24 ms            | 4.19 ms         | 199 µs          |

The new core scales at **≈ n¹·⁹**, the macro's exponent. The ~900× gap at
n = 64 is now a steady **~11× constant factor**, which is the dynamic-typing
tax ADR 0001 predicted. The conjunctive-query benchmark fell from 2.95 ms to
201 µs interpreted (≈ 15×) and to 390 µs under wasmtime.

The faster join core also changes 0008's finding that the expression backends
were "indistinguishable on join-heavy work". Join bookkeeping no longer drowns
the expression boundary, and the interpreter now beats wasmtime by about 2× on
both join and query workloads.

## Correctness

- **Differential oracle.** A new property, `fuzz::differential_seminaive`
  (`seminaive_oracle` in `tests/fuzz_diff.rs`), runs random stratified programs
  through the new evaluator and through a retained naïve reference loop
  (`eval::run_naive`: full re-fire, no indexes). The facts are split into two
  batches with a run after each, so the incremental path is checked against
  full recomputation on every case.
- **Existing suites.** The existing macro differentials (transitive closure,
  even/odd, shortest path over a lattice, distance-squared) and the wasm-vs-
  interpreter oracle pass unchanged.
- **Fail closed.** The iteration bound still applies per stratum, and hitting
  it is still `EvalError::IterationLimit`. A run that errors leaves the
  fixed-point marks where they were, so the next run re-covers everything
  since the last good fixed point.

## Consequences

- **The case for compiling rules is gone for now.** With the core at the
  macro's exponent, the remaining gap is a constant, and the interpreter beats
  the wasm tier on join-heavy work. Together with 0008 §3 (the interpreter beats
  the nested executor on `wasm32`), the default expression backend should be
  revisited: the interpreter wins on every workload measured.
- **Constant-factor headroom remains** and is the next lever, not codegen.
  Variable bindings are a `HashMap<Symbol, Value>` (the `ExprEval` seam takes
  one); `unify` allocates per match; index keys are `Vec<Value>`; a new tuple is
  copied into the row log, the dedup set, and the pending buffer. Join order is
  the written order: no reordering was attempted.
- **Memory.** Each derived tuple is stored twice (row log and dedup set), and
  each index adds a posting entry per row. Forks (`speculate`) clone indexes
  along with rows.
- **Observable differences.** `Database::tuples`/`Engine::query` now return
  rows in insertion order (previously hash-set order, so this was
  unspecified). `eval_query` rejects an unknown relation anywhere in the body
  up front, rather than only when traversal reached it. Under the multi-head
  stratum quirk, or with set relations derived from intermediate lattice values,
  which tuples appear was already schedule-dependent; the schedule is now
  Jacobi-style per rule firing rather than mid-iteration-visible.
