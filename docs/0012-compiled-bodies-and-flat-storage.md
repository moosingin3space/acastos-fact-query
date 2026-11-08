# 0012: Compiled rule bodies and flat relation storage

- **Status:** Accepted
- **Crate:** `ascent-jit` (internal; no public API change)
- **Builds on:** [0009](0009-semi-naive-indexed-evaluation.md) (the evaluator
  whose constant factors this attacks), [0010](0010-retire-the-wasm-expression-tier.md)
  (the interpreter as the only backend)

## Context

After 0009 the core scaled like the `ascent!` macro but ran about 11× slower
on transitive closure. 0009 listed the suspects: a `HashMap<Symbol, Value>`
environment, a `unify` that allocated per match, `Vec<Value>` index keys, and
every new tuple copied into the row log, the dedup set, and a pending buffer.

A counting allocator and timed prototypes confirmed them. There were about 13
allocations per derived tuple and 8 per fact on an expression-heavy rule, and
SipHash ran on every variable read and write, index probe, and dedup check. A
fast hasher alone cut about a third off every workload. Allocation-free
matching on top of it cut another quarter off joins and queries. Rather than
tune the hash-and-allocate design, this record replaces it with flat arrays.

## Decision

**1. Rule bodies are compiled against static binding information.** A body is
evaluated left to right, so which variables are bound at each clause is known
at planning time. `compile.rs` turns each body into steps over a flat
`[Value]` environment:

- **Variables become dense slots.**
- **Matching a row** is a list of per-column operations: `Skip`, `Lit`,
  `Bind(slot)`, or `Check(slot)`. It needs no hashing and no undo log, because
  planning never reads a slot that is not bound at that point.
- **Expressions compile to postfix bytecode** run on a reusable value stack.
  `&&` and `||` are forward jumps. There is no recursion at run time.
- **The tree-walking `Expr::eval` remains the public API** and the oracle the
  bytecode is fuzzed against. `fuzz::bytecode_matches_tree` requires the same
  value or error, including unbound-variable errors raised lazily through
  short-circuiting.

**2. Shadowing follows Ascent's scoping.** A `let` or an aggregate output binds
a fresh slot that shadows any outer variable for the rest of the body, and an
aggregate's source-local variables are visible only to its argument. This
fixes a latent bug. Previously `let x = …` over an already-bound `x` removed
the outer binding on backtrack, so a later row could *bind* where it should
have *checked*. `tests/semantics.rs` pins the new behaviour. The fuzz
generator emits no `let`, so the differential oracles were unaffected.

**3. Relations are flat.** Each relation is:

- **one `Vec<Value>` row log** (row-major, arity stride);
- **a liveness bit set**;
- **`KeyTable`s** (`table.rs`): open addressing over row ids, mapping a
  projection of the stored rows to a group. Lookups compare the probe key
  against a representative row in place.

Set deduplication (all columns), lattice keys (each group points at the key's
live row), and every index (groups → sorted posting lists) are one table each.
A tuple is stored exactly once.

**4. Hashing is seeded.** `table.rs` hashes values with a folded 64×128-bit
multiply, seeded once per process from std's `RandomState`. Facts and queries
may be untrusted, and an unseeded fast hash would let an attacker precompute
colliding keys and turn indexed joins quadratic: a time-based denial of service
that the cardinality caps do not bound.

**5. Smaller measured fixes.**

- **Pending tuples** (the buffer of head tuples awaiting insertion) are
  deduplicated by sorting when the buffer doubles, replacing a parallel hash
  set. This is deterministic, and memory stays within twice the number of
  distinct new tuples.
- **The body walk checks the stack every 8th level** instead of every level,
  as expression evaluation already did.

## Evidence

Same machine and caveats as 0008/0009. From `cargo bench`, against a baseline
saved immediately before:

| Benchmark | Before | After | Change |
|---|---:|---:|---:|
| Transitive closure, n=64 | 2.26 ms | 612 µs | −73% |
| Conjunctive query | 202 µs | 73 µs | −63% |
| Expression-heavy run | 1.61 ms | 735 µs | −54% |

The gap to the macro on transitive closure is now **3.1×** (it was ~11×).
Allocations per iteration in the profiling harness fell 5.7× (transitive
closure, n=128), 2.7× (expression-heavy), and 2.6× (query). Under Node, chain
n=64 went from 2.52 ms to 0.70 ms; the boundary-dominated query improved
less, from 9.1 ms to 6.8 ms.

## Correctness

- **All existing suites pass.** That covers the macro differentials (including
  the lattice shortest-path one, which exercises tombstoning), the semi-naïve
  oracle, and the limits tests.
- **The new bytecode property passes.**
- **The fuzz suite ran 20 s per property.**
- **The naïve reference loop shares the new storage**, so storage bugs are
  caught by the macro differentials and tests rather than by that oracle.

## Consequences

- **Depth errors are now raised when planning, not when evaluating.** A rule
  or query whose expression exceeds `MAX_EXPR_DEPTH` fails at
  `run`/`explain`/`evaluate_query` even if the expression would never have
  been reached. This is stricter and still fails closed.
- **Where the time goes now** (samply):
  - **Expression-heavy rules:** bytecode dispatch, about 45%, from
    tagged-value checks and `Result`-returning helpers.
  - **Joins:** index probing.
  - **Queries:** materializing `QuerySolution` provenance, which clones a
    relation name `String` and a tuple `Vec` per supporting row, and rebuilding
    transient indexes per call.
  - **Loading facts:** each `add_fact` hashes the relation name with SipHash.

  These are the next levers.
