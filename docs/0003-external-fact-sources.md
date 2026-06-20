# 0003: A pluggable external fact-source seam

- **Status:** Accepted
- **Crate:** `fact-query`
- **Builds on:** [0001](0001-ascent-jit-runtime-engine.md) (the value model,
  provenance, and fork-and-discard evaluation) and
  [0002](0002-fact-query-substrate.md) (the `FactStore` query seam and the
  "provenance is the engine's, policy is the app's" discipline this must not
  break).

## Context

The engine often has to consume facts it did not author. One shape is *streaming*
— a connector pulls from the world and asserts facts one at a time. A second,
structurally different shape is a **batch, schema-described, content-addressed
fact base** produced by an *analyzer* run over a corpus — for example a static
index over a body of source, or a structured extraction over a document set.

Such a producer emits **typed tuples in bulk**, carries its **own schema** (stable
predicate identifiers, column types, per-schema fingerprints), and is
**content-addressed and immutable** (a unit of facts is named by the hash of its
bytes). It is derived from artifacts that are **hostile by default** (external
content), so its facts are no more trustworthy than the corpus they came from.

Without a common seam, every such producer is a hand-written loader. That is three
problems wearing a trench coat:

1. **No common seam.** Each producer is wired by hand; nothing is reusable.
2. **No schema contract.** The engine validates *arity* at load but knows nothing
   of **column types** or **schema drift**. A producer whose schema shifts under
   it corrupts decisions silently.
3. **An impedance mismatch with the value model.** The engine's `Value` is a
   closed, `Copy` `{Int(i64) | Bool | Sym}` with interned strings. External tuples
   carry rich types (wide identifiers, fixed-width content hashes, enums, names);
   how they become `Value`s is unspecified, and getting it wrong is a correctness
   bug in a *decision input*.

This record adds the missing seam, the schema contract, and the lowering —
**without** weakening the value model and **without** making the engine learn any
application's trust vocabulary.

## Decision drivers

- **One typed seam, many producers.** A new producer should plug in, not be
  hand-wired, and the seam belongs with the reusable substrate, carrying no
  application policy.
- **The engine must gain a real schema contract.** Column types and schema-drift
  detection are the price of consuming facts the engine did not author. Arity-only
  validation is not enough once the producer's schema can move.
- **Don't crack open `Value`.** The closed, `Copy` value model is load-bearing —
  hashing, the WASM bit-bridge, and the pure-interpreter differential oracle all
  rest on it. Lowering must fit the existing three kinds.
- **Governance stays above the substrate.** The engine and the seam own
  *provenance*; trust/origin is the app's, expressed in user rules. The seam must
  not learn an application's origin or denial vocabulary.

## Decision

### A `FactSource` backend trait, distinct from the `FactStore`

A `FactSource` trait *produces* facts into the engine's fact base, kept distinct
from the `FactStore` of [0002](0002-fact-query-substrate.md), which *queries* an
already-materialized one. They **compose**: a `FactSource` populates the fact base
a `FactStore` then queries. `FactSource` lives in `fact-query` and depends on
nothing application-specific — the same governance-free guarantee, enforced the
same way, by dependency direction.

```rust
pub trait FactSource {
    type Error;

    /// One descriptor per predicate: a stable id, a namespaced name, a
    /// per-predicate schema fingerprint, ordered column types, and a required
    /// doc-string (doc-strings are load-bearing for grounding).
    fn schema(&self) -> &[PredicateDescriptor];

    /// A stable content identity for this batch, computed over the producer's
    /// own bytes *before* any interning. The cache key and the origin handle.
    fn content_id(&self) -> ContentId;

    /// Stream the producer's tuples, already lowered into the engine's value
    /// model, interning symbol columns into the caller's interner. Bulk by
    /// design.
    fn tuples(&self, interner: &mut Interner) -> Result<TupleStream<'_>, Self::Error>;
}
```

What is **absent** is the point: no commit path, no namespace opinion, no trust
tag. Where the tuples land, what origin they are stamped with, and whether they
may justify an action are **the host's** decisions, applied above the seam.

### The schema contract: column types and drift detection

A `PredicateDescriptor` upgrades what is validated at ingest from *arity* to
**arity + column types + fingerprint**:

- **Column types** are checked against the relation declaration. A producer column
  whose lowered type does not match is rejected at ingest, not discovered as a
  wrong answer later.
- **A per-predicate schema fingerprint** lets the host detect **schema drift**: a
  producer built against a schema this binary no longer agrees with is refused.
  This is the column-grain analogue of the arity check. Two different schemas can
  lower to the same column types (a field rename, a column whose meaning moved),
  and only the fingerprint distinguishes them — so columns are checked first as
  the more actionable diagnostic, and the fingerprint last as the catch-all.

### Lowering: rich external tuples into the closed value model

External columns are mapped, **per predicate, by a total and deterministic
lowering**, into the existing `{Int(i64) | Bool | Sym}`; the value model is not
widened:

- **Identity tokens become `Sym`** — names, wide identifiers, fixed-width content
  hashes, enum tags. These are compared and joined, never used in arithmetic;
  interning them sidesteps the `i64` range problem for identifiers wider than 63
  bits and keeps enum semantics off ordinal values. A composite identity (e.g. a
  `(source, local-id)` pair) lowers to **multiple columns** so rules can join on
  the parts, rather than to one opaque blob.
- **Numerics become `Int`**, **flags become `Bool`**.

The consequence is that most external columns lower to `Sym` and the interner does
the heavy lifting — which is exactly why the closed value model survives contact
with rich external schemas.

### Content identity is pre-interning; interner ids never escape

The engine's interner assigns symbol ids **by insertion order**: stable *within*
one engine instance, not across runs. Therefore a producer's `content_id` is
computed over its **own bytes, before interning**, and is the only thing used for
caching, origin tagging, and any cross-run comparison. A symbol id **must never**
leak into a content hash, a cache key, or the differential oracle — that would
make identity run-dependent. This is a standing invariant, the same shape as
[0002](0002-fact-query-substrate.md)'s "no policy leaks downward": here, *no
interner state leaks into identity.*

### Content identity as a cache key

Because a producer is content-addressed, an unchanged `content_id` means an
unchanged set of facts, so the host may **skip re-lowering and re-ingesting** a
producer whose identity is unchanged. This is an enabling step toward incremental
ingestion; it does **not** by itself deliver incremental *evaluation*, which
remains future work.

## Considered alternatives

- **Widen `Value` to carry rich types.** Rejected. It breaks `Copy`, hashing, the
  WASM bit-bridge, and the differential oracle — a large blast radius across the
  evaluation core — to buy what lowering-to-`Sym` already achieves.
- **A record/row column (a composite value in one column).** Rejected for this
  seam. The composites seen in practice are *identities* used as opaque keys
  (equality + hashing only) whose components are *joined on elsewhere* — and a
  record would block exactly those joins, then need a field-projection operator
  whose cost lands on the WASM tier and the dual-backend differential contract.
  Records earn their place for genuinely **recursive / variable-arity** payloads,
  and even then only as interned, construct/destructure-only handles — deferred
  until a workload with nested data earns it.
- **Zero-copy: borrow the producer's archive directly as engine relations.**
  Deferred. The engine's relations own their tuples and the speculative model
  clones the whole database per fork; borrowing a foreign archive for the engine's
  lifetime fights both. Revisit only if ingestion copy cost is measured to
  dominate.
- **Bake the schema/lowering into the engine.** Rejected. Lowering and schema
  description are substrate concerns; the engine validates against relation
  declarations it already has and stays ignorant of the rest.
- **Reuse the `FactStore` as the ingestion seam too.** Rejected as a category
  error: `FactStore` is read/query, `FactSource` is produce/write-into-fact-base.
  Conflating them smuggles a write concern into a seam kept deliberately read-only.

## Consequences

### Positive

- Heterogeneous batch producers plug into one typed, governance-free seam.
- The engine gains a real schema contract — column-type validation and
  schema-drift detection — where before it checked only arity.
- Content addressing pays twice: it is the origin handle for free and the cache
  key that blunts re-ingestion cost.
- The closed value model survives rich external schemas, because identity-token
  columns lower to interned `Sym`s rather than forcing `Value` open.

### Negative / risks

- **Ingestion copies.** Lowering materializes external bytes into owned engine
  tuples; a bulk producer pays a full copy per session. Accepted for now; the
  deferred zero-copy alternative is the escape hatch if measured to dominate.
- **Lowering is a semantic surface, and lossy by construction.** Collapsing rich
  types into three kinds has real ambiguity. A sloppy per-predicate spec is a
  correctness bug in a decision input; each predicate's lowering must be specified
  and tested, not improvised.
- **The producer parser is attack surface.** It decodes content derived from
  hostile artifacts, inside the decision path. It must be fuzzed, and malformed
  input must **fail closed**, never partially ingest.
- **Interner-leak is a latent nondeterminism bug.** The pre-interning
  `content_id` rule is the guard; it must be held.
- **This does not deliver incremental maintenance** — only the cache key toward
  it.
