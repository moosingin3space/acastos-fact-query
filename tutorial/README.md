# Modeling data in Acastos

A hands-on guide to shaping data so the engine can actually reason over it.

The design records in [`../docs`](../docs) explain *why* the substrate is built
the way it is. This is the other genre: *how* to use it well. It teaches one
skill — turning the data you have into relations the engine can join, derive
over, and explain — and it teaches it by repeatedly fixing the **single most
common modeling mistake**: hiding structure inside a value.

## The one idea

The engine's value model is **closed and tiny**: every column is an
`Int(i64)`, a `Bool`, or an interned `Sym` (a string, compared by identity). It
will never grow an array, a record, or a nested type — that closure is
load-bearing (see [docs/0003](../docs/0003-external-fact-sources.md)). So the
question is never "what type do I add?" It is always:

> **How do I lower this structure into relations over `{Int | Bool | Sym}`?**

Get that lowering right and the relational core does the rest. Get it wrong —
flatten a list or a record into one `Sym` — and you have data the engine holds
but cannot reason about: joins you can't write, derivations that don't exist,
provenance that can't explain what you know is true.

## The parts

Read them in order; each builds the running example from the previous one.

| # | Part | What you learn |
|---|------|----------------|
| 1 | [Foundations](01-foundations.md) | The closed value model, relations vs. facts vs. rules, and the read-only query grain with provenance and a cardinality bound. |
| 2 | [Don't flatten structure](02-modeling-structure.md) | The flattening anti-pattern (an argument vector stuffed into one `Sym`), why it kills joins, and the two fixes: composite identity → multiple columns, ordered sequence → an indexed side relation. |
| 3 | [Collections, keys, and grounding](03-collections-keys-grounding.md) | Ordered vs. unordered collections, bounded aggregation, keeping key-spaces consistent so relations join, and using doc-strings + provenance to keep the model honest. |

## Running the snippets

Examples are Rust, against the `ascent_jit` engine and the `fact_query` query
grain — the same two crates the rest of the repo is built on. Each complete
`rust` block below is compiled and run as a doctest, so the tutorial cannot drift
from the real API. What matters most in every snippet is the **Datalog text**
(the schema, the facts, the queries): that is the thing you are actually
designing, and it is identical whether you drive it from Rust, the WebAssembly
binding, or anything else.

```rust
use ascent_jit::{Engine, Value};
use fact_query::{Cardinality, FactStore};

// The schema — relation declarations and rules — is the program.
let mut engine = Engine::from_source("relation invocation(sym, int);").unwrap();

// Ground facts enter through add_fact, never through the program text. An `int`
// column is a `Value::Int`; a `sym` column is an interned `Value::Sym`.
let argv = Value::Sym(engine.intern("list"));
engine.add_fact("invocation", vec![argv, Value::Int(0)]).unwrap();
engine.run().unwrap();

// A query is `(outputs) <-- body`; it is parsed, form-checked, then evaluated
// read-only under a cardinality cap, returning rows with provenance.
let q = engine.parse_query("(a) <-- invocation(a, 0)").unwrap();
engine.check(&q).unwrap();
let (results, _provenance) = engine.eval(&q, Cardinality::new(1_000)).unwrap();

assert_eq!(results.rows().len(), 1);
assert!(!results.is_truncated());
```

Note the two regions, enforced by the engine's parser: **relations and rules**
go in `from_source`; **ground facts** enter through `add_fact`. A symbol value is
built by interning its string (`Value::Sym(engine.intern("list"))`); an integer
is `Value::Int(0)`. Schema rules are terminated with `;`.
</content>
