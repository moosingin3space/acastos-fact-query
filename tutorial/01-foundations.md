# Part 1 — Foundations

Before we can model anything *well*, we need the vocabulary: what a value is,
what a relation is, how facts and rules differ, and what a query gives back. The
running example throughout this guide is **characterizing a command-line tool** —
recording what each invocation does so the behavior can later be verified.

## Values: three kinds, and that's all

A column holds a [`Value`](../ascent-jit/src/value.rs), and a `Value` is exactly
one of three things:

- `Int` — a signed 64-bit integer. Covers exit codes, counts, sizes, anything
  arithmetic.
- `Bool` — a flag.
- `Sym` — an **interned string**: stored once, compared and hashed as an integer
  id. Names, paths, identifiers, command names, output text — all symbols.

That is the whole type system. There is no string-with-operations, no list, no
record, no null. The reason is spelled out in
[docs/0003](../docs/0003-external-fact-sources.md): the closed, `Copy` value
model is what lets the engine hash tuples cheaply, bridge expressions through a
single `i64` into its WebAssembly tier, and check itself against a pure
reference interpreter. Widening it would break all three. So rich data does not
get a new type — it gets **lowered** into these three kinds. Learning to do that
lowering well is the entire skill, and Parts 2 and 3 are about little else.

In Rust, you build the three kinds like this — a symbol is interned through the
engine, which is what turns a string into an integer-comparable id:

```rust
use ascent_jit::{Engine, Value};

let mut engine = Engine::from_source("relation r(int, bool, sym);").unwrap();

let n: Value = Value::Int(42);
let flag: Value = Value::Bool(true);
let name: Value = Value::Sym(engine.intern("list")); // string -> interned symbol

engine.add_fact("r", vec![n, flag, name]).unwrap();
```

A crucial consequence to internalize now: **a `Sym` is an atom of identity, not
a string you can look inside.** Two symbols are equal or they are not; you can
join on them and hash them. You cannot, inside a rule, take its length, split it,
index a character, or test a prefix. The string is gone the moment it is
interned; only its identity remains. Every modeling mistake in Part 2 is a
version of forgetting this.

## Relations, facts, and rules

A **relation** is a named, typed set of tuples — a table whose column types are
fixed at declaration:

```text
relation invocation(sym, int);        % an argument line, and the exit code it produced
relation output_contains(sym, sym);   % an argument line, and a substring seen in its output
```

A **fact** is a ground tuple — a row with no variables — asserted into a
relation. A **rule** derives new tuples from existing ones; it has a head and a
body, `head <-- body`, and fires for every assignment of variables that makes
the body true. Schema rules end with `;`:

```text
relation characterized(sym);
characterized(argv) <-- invocation(argv, _);   % every argv we have an exit code for
```

The engine enforces a hard split, and so will your code: **relations and rules
are the program** (they go through `from_source`); **facts are data** (they enter
through `add_fact`). The parser rejects a body-less rule, so a ground fact
*cannot* live in the program text — which is exactly why the two never get
confused.

```rust
use ascent_jit::{Engine, Value};

let mut engine = Engine::from_source("
    relation invocation(sym, int);
    relation output_contains(sym, sym);
    relation characterized(sym);
    characterized(argv) <-- invocation(argv, _);
").unwrap();

let list = Value::Sym(engine.intern("list"));
engine.add_fact("invocation", vec![list, Value::Int(0)]).unwrap();

let check = Value::Sym(engine.intern("check 999"));
engine.add_fact("invocation", vec![check, Value::Int(1)]).unwrap();
let check = Value::Sym(engine.intern("check 999"));
let msg = Value::Sym(engine.intern("Invalid todo number: 999"));
engine.add_fact("output_contains", vec![check, msg]).unwrap();

engine.run().unwrap(); // derive characterized(...) to a fixed point

assert_eq!(engine.query("characterized").len(), 2);
```

`run()` evaluates every rule to a **fixed point** — it keeps deriving until
nothing new appears. Call it after asserting facts and before querying, so the
query sees the materialized state.

## Querying: read-only, with provenance, under a bound

A query is a conjunctive expression — joins and filters over existing relations —
written `(outputs) <-- body`. A filter is an `if` clause; a literal like `0` is a
legal argument:

```rust
use ascent_jit::{Engine, Value};
use fact_query::{Cardinality, FactStore};

let mut engine = Engine::from_source("
    relation invocation(sym, int);
    relation output_contains(sym, sym);
").unwrap();

let check = Value::Sym(engine.intern("check 999"));
engine.add_fact("invocation", vec![check, Value::Int(1)]).unwrap();
let check = Value::Sym(engine.intern("check 999"));
let msg = Value::Sym(engine.intern("Invalid todo number: 999"));
engine.add_fact("output_contains", vec![check, msg]).unwrap();
engine.run().unwrap();

// Which argument lines failed (non-zero exit) AND printed a diagnostic?
let q = engine
    .parse_query("(argv, m) <-- invocation(argv, code), output_contains(argv, m), if code != 0")
    .unwrap();
engine.check(&q).unwrap();
let (results, provenance) = engine.eval(&q, Cardinality::new(10_000)).unwrap();

assert_eq!(results.rows().len(), 1);
assert!(!results.is_truncated());
// Provenance says *why*: the invocation and output_contains facts that joined.
assert_eq!(provenance.rows()[0].justifications[0].support.len(), 2);
```

Three properties of this query are worth naming now, because the rest of the
guide leans on them:

- **It is read-only.** A conjunctive query has no write semantics; it cannot
  mutate the fact base. You can run any number of them without changing state.
- **It returns provenance.** Alongside each row, `provenance` records *which
  facts joined to produce it* — here, the specific `invocation` and
  `output_contains` tuples. Provenance is the engine's, and it is also your best
  tool for auditing whether your **model** is right (Part 3).
- **It is cardinality-bounded.** Every query is capped on the number of
  solutions. If the cap is hit, `results.is_truncated()` is `true` — a **surfaced
  outcome, never a silent cut**. A bound that is hit and ignored is a
  memory-exhaustion bug; the contract makes you see it. (See the five guarantees
  in [`../fact-query/README.md`](../fact-query/README.md).)

One honest caveat, carried from that same contract: the engine guarantees your
query is *well-formed* — parsed, schema-valid, safe, read-only, bounded. It does
**not** guarantee the query *answers the question you meant to ask*. Form, not
meaning. Provenance and a human net close that gap; the engine does not pretend
to.

## What you have now

You can declare typed relations, assert facts, derive with rules, and ask
read-only questions that come back with evidence and a bound. That is enough to
model *something* — but not yet to model it *well*. The very next thing most
people do is reach for structure the value model doesn't have: they take a list
or a record and stuff it into a single `Sym`. That is
[Part 2](02-modeling-structure.md).

---

**Part 1 in one line:** values are `{Int | Bool | Sym}`, a `Sym` is identity not
text, relations+rules are the program while facts are data, and every query is
read-only, evidenced by provenance, and bounded.
</content>
