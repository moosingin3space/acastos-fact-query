# fact-query

A **governance-free** proposer/verifier substrate over an
[`ascent-jit`](../ascent-jit) fact base.

It captures one recurring shape of neurosymbolic systems: **an untrusted
artifact is proposed, the deterministic engine speculatively evaluates it under a
resource bound and reports what it *does* (with provenance), and some net — a
human, a vote, or nothing — decides whether that is what was wanted.**

This crate carries **no policy**: no LLM, no commit path, no denial vocabulary,
no human gate. It depends on `ascent-jit` and **never** on any application. That
dependency direction *is* the governance-free guarantee — an application's writer
model, denial vocabulary, and trust/taint lattice all layer on top, never leak
down.

## v1: the queries grain

v1 ships exactly one grain — **conjunctive queries**: joins, filters, and
aggregates over existing relations. No new derived relations, no recursion, no
negation.

```rust
use ascent_jit::{Engine, Value};
use fact_query::{Cardinality, FactStore};

let mut engine = Engine::from_source("relation edge(int, int);").unwrap();
engine.add_fact("edge", vec![Value::Int(1), Value::Int(2)]).unwrap();
engine.add_fact("edge", vec![Value::Int(2), Value::Int(3)]).unwrap();
engine.run().unwrap();

// Parse -> form-check -> bounded, read-only evaluation.
let query = engine.parse_query("(x, z) <-- edge(x, y), edge(y, z)").unwrap();
engine.check(&query).unwrap();
let (results, provenance) = engine.eval(&query, Cardinality::new(10_000)).unwrap();

assert_eq!(results.rows().to_vec(), vec![vec![Value::Int(1), Value::Int(3)]]);
assert!(!results.is_truncated());

// Provenance says *why*: the two edges that joined to yield the row.
assert_eq!(provenance.rows().len(), 1);
assert_eq!(provenance.rows()[0].justifications[0].support.len(), 2);
```

## The contract — five guarantees, one disclaimer

For an evaluated query the substrate guarantees, deterministically and with **no
LLM in the verification loop**, that it is:

1. **Parsed** — valid query IR.
2. **Schema-valid** — every referenced relation exists, arity matches, column
   types match.
3. **Safe / range-restricted** — every output variable and every variable in a
   filter is bound by a positive body literal.
4. **Read-only** — guaranteed by the query class. A conjunctive query has no
   write semantics, so it cannot mutate persisted state *at the grammar level*,
   not by handle plumbing.
5. **Cardinality-bounded** — evaluation cannot blow up space past the supplied
   `Cardinality` cap. The bound is on **cardinality, not time** (a conjunctive
   query always terminates; the only blast radius is space). **Hitting the cap is
   a first-class outcome** (`ResultSet::is_truncated`), surfaced — never an error
   to paper over. A forgotten cap is a memory-exhaustion DoS.

### What it does **not** guarantee

> **The substrate does _not_ guarantee that a query answers the question asked.**
> It checks **form**, not **meaning**. `eval` returns a `ResultSet`, never an
> `Answer`.

A query can parse, type-check, run, and return a clean result set that does not
mean what was intended — the **"valid but wrong"** problem that dominates the
text-to-query literature. The substrate's contribution against it is
**`Provenance`** (which facts joined to produce each row) and this honest
disclaimer — nothing more.

Closing the gap is the **caller's net** to build on top: show-the-evidence-and-
confirm, a paraphrase round-trip, N-candidate self-consistency. **Do not let this
crate's clean API read as authoritative about answers.**

## Grounding precondition

Writing a correct query over an arbitrary schema requires the proposer to know
that schema — ideally the *meaning* of each relation, not just its arity. So
**relation doc-strings are load-bearing**: `Schema` carries an optional `doc` per
relation. The `ascent-jit` backend reports `None` today (its IR has no
doc-strings yet); that is a known grounding gap, not a contract violation. A fact
base of cryptic relation names with no descriptions is close to ungroundable.

## Backends

The query seam is the `FactStore` trait. `ascent-jit`'s `Engine` gets the
canonical implementation; a different backend (another Datalog engine, a
relational store) can implement the same trait, which is what makes any loop
written over it portable.

The complementary **produce** seam is `FactSource` — a backend that describes a
batch of facts (schema, content identity) and streams them already lowered into
the engine's value model. The two compose: a `FactSource` populates the fact base
that a `FactStore` then queries.

## Design

See [`../docs`](../docs) for the design records that motivate the substrate, the
five-guarantee contract, and the `FactStore` / `FactSource` seams.

## Not in v1

The **facts** grain ("if these tuples were asserted, what fires?") and the
**rules** grain ("if this inference existed, what would it derive?") are deferred
to later grains of the same crate, added as consumers earn them. The LLM
**synthesis + grounding** layer is deliberately *not* part of this crate — it is
an application concern that layers on top.
