# 0013: Normalized provenance and independent row / evidence caps

- **Status:** Accepted
- **Crates:** `ascent-jit`, `fact-query`, `fact-query-node`
- **Revises:** [0002](0002-fact-query-substrate.md) (guarantee 5's single
  cardinality cap becomes two independent caps; provenance changes shape)
- **Builds on:** [0012](0012-compiled-bodies-and-flat-storage.md) (append-only
  row logs, which make row ids stable fact handles)

## Context

Provenance crossed every layer as a fully nested, by-value tree:

- per row, a list of justifications;
- per justification, a list of `(relation name, tuple)` pairs;
- each pair a fresh `String` and `Vec`.

Profiling the Node benchmark's query (780 rows) put about 40% of the engine's
query time into building that tree (6 allocations per solution). Grouping in
`fact-query` added 230 µs, and turning it into JS objects took about 5.9 of the
6.8 ms `query()` total.

Most of it was redundant:

1. **Relation names were repeated per supporting fact.** Every justification
   has exactly one supporting fact per positive body atom, in body order, so
   position `i` is always the same relation for a given query.
2. **Shared facts were copied once per use.** A fact supporting 50 rows was
   copied 50 times.
3. **The result row was stored again** inside the row's own provenance.

The single cap had a semantic problem as well. `max` capped *join
combinations*, so one row with many justifications could exhaust it, and a
caller could not tell "there may be more answers" from "these are all the
answers, but not every reason".

## Decision

1. **Two independent caps.** Every query takes a row cap and a per-row
   justification cap: `QueryLimits` in the engine, `fact_query::Limits` in the
   substrate, and `{ maxRows, maxJustificationsPerRow }` in JS. The defaults are
   10 000 rows and 16 justifications per row. Each cap has its own flag, and
   each flag means *may be incomplete*:

   - `rows_truncated` / `ResultSet::is_truncated` / `truncated`: more rows may
     exist.
   - `evidence_truncated` / `Provenance::is_truncated` /
     `provenance.truncated`: some returned row may have more justifications.

   Reaching the row cap stops evaluation and sets *both* flags, because the
   remaining evidence was not examined. `false` always means complete, which is
   what a fail-closed caller needs. Reaching only the per-row cap keeps
   evaluating, so that every row is still found.

2. **Normalized, flat provenance.** The engine groups solutions into distinct
   rows itself (it must, to count rows) and returns:

   - `atoms`: the relation of each support column;
   - `facts`: each distinct supporting fact, once;
   - `offsets`: CSR row offsets into the justifications;
   - `support`: per justification, `k` indices into `facts`.

   `fact-query` keeps the same normalized shape, with views
   (`justifications(row)`, `support()`, `FactView`) rather than materialized
   nesting. The Node binding ships it nearly as-is: the facts table once, and
   `offsets`/`support` as `Uint32Array`s. The TS `Provenance` converts each
   distinct fact once, and `justifications(row)` returns arrays of the shared
   fact objects.

3. **Fact handles.** A supporting fact is an opaque `FactRef` (relation, row).
   Row logs are append-only and superseded lattice rows are only marked dead, so
   a handle stays valid and names the same tuple for the life of its database.
   `Engine::fact` resolves a handle, `Derivation.support` is now handles too,
   and `Engine::explain_fact` explains a fact by handle, so a derivation tree
   can be walked step by step without copying values. Handles stay engine-level:
   `fact-query`'s trait is backend-neutral and has no `explain`, so its
   provenance carries values. Handles are meaningless across engines; durable or
   cross-engine identity is still [0003](0003-external-fact-sources.md)'s
   content addressing.

## Evidence

Same machine and caveats as 0008/0009/0012:

| | Before | After |
|---|---:|---:|
| `cargo bench` conjunctive query | 73 µs | 53 µs |
| Engine query, Node-bench workload | 627 µs, 4 904 allocs | 585 µs, 313 allocs |
| `fact-query` check + eval, same | 858 µs, 7 279 allocs | 678 µs, 1 892 allocs |
| Node `query()`, 780 rows | 6.8 ms | 3.3 ms |

On that workload the engine's time is dominated by the 12 000-combination join
itself, so its allocations fall 94% while its time falls 7%. The gain shows at
the boundary.

## Consequences

- **Breaking, pre-1.0.**
  - `ascent-jit`: `QuerySolution` is gone, `QueryOutput` is reshaped, and
    `Derivation.support` is `Vec<FactRef>`.
  - `fact-query`: `Cardinality` becomes `Limits`, and `RowProvenance` /
    `SupportTuple` / owned `Justification` become `Provenance` views plus
    `SupportFact` / `FactView`.
  - npm package: `QueryResult.provenance` is a `Provenance` object, and
    `query()`'s second argument accepts `QueryLimits` (a number still means max
    rows). `DEFAULT_MAX_CARDINALITY` remains as a deprecated alias for
    `DEFAULT_MAX_ROWS`.
- **Time is still unbounded by the caps, as before.** Space is bounded by
  `max_rows × max_justifications_per_row`. Reaching only the per-row cap does
  not stop the join, which must keep looking for new rows; a single cap on
  combinations used to stop earlier in that case. Conjunctive queries
  terminate, and a filtered cartesian product with no solutions was never
  bounded by the cap either.
- **Room for absence evidence.** `atoms` describes body positions. If a later
  grain admits negation, those positions can gain a kind ("checked absent")
  without another format break.
- **Remaining JS-side cost** is mostly converting rows and facts. Integers
  cross as decimal strings and become `bigint`s.
