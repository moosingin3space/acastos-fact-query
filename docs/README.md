# Design records

These documents record the design decisions behind the two crates in this
workspace and the reasoning that led to them. They are written to be read in
order, but each stands on its own.

| # | Title | What it covers |
|---|-------|----------------|
| [0001](0001-ascent-jit-runtime-engine.md) | A runtime interpreter for Ascent | Why the engine runs Datalog supplied *as data*, the interpreter + WebAssembly expression tier, provenance, the bounded fixed point, and speculative evaluation. |
| [0002](0002-fact-query-substrate.md) | The `fact-query` proposer/verifier substrate | The recurring propose → speculate → (delta, provenance) → net primitive, the governance-free boundary, the queries grain, and the five-guarantee contract. |
| [0003](0003-external-fact-sources.md) | A pluggable external fact-source seam | The produce-side `FactSource` seam, the schema contract (column types + drift detection), lowering rich external tuples into the closed value model, and content-addressed identity. |

## Conventions

- **Status** is one of *Proposed*, *Accepted*, or *Superseded*.
- A record is a snapshot of the reasoning at a point in time. Later records may
  build on or revise earlier ones; they say so explicitly when they do.
- The records describe *why*, not *how* — the code and its doc-comments are the
  authority on the current API.
