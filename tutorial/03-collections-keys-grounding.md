# Part 3 — Collections, keys, and grounding

Part 2 turned a flattened argument vector into an indexed side relation. This part
finishes the modeling story: when a collection needs that index and when it
doesn't, how to derive over a collection without blowing the bound, how to keep
two relations about the same thing joinable, and how to use doc-strings and
provenance to keep the whole model honest.

## Ordered vs. unordered: earn the index column

Both of these are "a collection of things attached to an invocation," and they
look similar — but they want different shapes:

```text
relation arg(sym, int, sym);          % the argument vector: an ORDERED sequence
relation output_contains(sym, sym);   % substrings seen in the output: an UNORDERED set
```

`output_contains("check 999", "Invalid todo number: 999")` is a **set member**.
Order doesn't matter — "the output contains X and Y" is the same as "Y and X" —
and you only ever ask membership or join questions of it. So it needs **no index
column**: one row per element, the owning identity plus the element, done.

`arg("rename 2 new task name", 1, "2")` is a **sequence element**. Position
*is* meaning: token 0 is the command, a flag at position 3 belongs to a different
argument than one at position 1, and `"a" "b"` is not `"b" "a"`. So it needs the
**index column** from Part 2.

> **Rule of thumb:** model a collection as a side relation either way. Add an
> index column **iff position or duplicates carry meaning** (a sequence). Omit it
> when the collection is a set — membership and joins, order irrelevant. Don't add
> an index you never join on; it's noise that invites bugs (two rows that should
> be equal aren't, because their indices differ).

## Deriving over a collection — and staying bounded

Because a collection is just rows, you summarize it with the aggregate grain.
"How many arguments did this invocation have?" and "what's its last index?" are
aggregations over `arg`:

```text
relation arg_count(sym, int);
relation arg_len(sym, int);
arg_count(argv, n)   <-- arg(argv, 0, _), agg n = count() in arg(argv, _, _);
arg_len(argv, last)  <-- arg(argv, 0, _), agg last = max(i) in arg(argv, i, _);
```

The surface form is `agg <out> = <func>(<expr>) in <atom>`, with `count`, `sum`,
`min`, and `max` available. This is how you recover "length"-like facts about a
sequence without ever needing a length *operator* on a value — you derive it from
the rows.

Note the leading `arg(argv, 0, _)` literal: it is what **binds the group key**.
The aggregate filters its source by the variables already bound in the rule, so
`argv` has to be established by a positive literal *before* the `agg` clause —
here, "every invocation that has a token at position 0." Drop that literal and
`argv` is unbound in the head, which the engine rejects. The `agg` clause
contributes only its output (`n`, `last`); it does not export the source's
bindings.

```rust
use ascent_jit::{Engine, Value};

let mut engine = Engine::from_source("
    relation arg(sym, int, sym);
    relation arg_count(sym, int);
    relation arg_len(sym, int);
    arg_count(argv, n)  <-- arg(argv, 0, _), agg n = count() in arg(argv, _, _);
    arg_len(argv, last) <-- arg(argv, 0, _), agg last = max(i) in arg(argv, i, _);
").unwrap();

let line = "rename 2 new task name";
for (i, tok) in ["rename", "2", "new", "task", "name"].iter().enumerate() {
    let id = Value::Sym(engine.intern(line));
    let t = Value::Sym(engine.intern(tok));
    engine.add_fact("arg", vec![id, Value::Int(i as i64), t]).unwrap();
}
engine.run().unwrap();

let id = Value::Sym(engine.intern(line));
assert!(engine.query("arg_count").contains(&vec![id, Value::Int(5)])); // 5 tokens
let id = Value::Sym(engine.intern(line));
assert!(engine.query("arg_len").contains(&vec![id, Value::Int(4)]));   // last index 4
```

Keep one invariant from Part 1 in view while you do this: **every evaluation is
cardinality-bounded and fails closed.** A query that would materialize an
unbounded cross-product over collection elements (say, joining `arg` to itself
several times unguarded) can hit the cap — and when it does, `is_truncated()` is
`true`, which a safety-conscious caller must treat as "I don't know," never as
"nothing matched." Model so the interesting answers fit comfortably under the
bound, and treat a truncation as the surfaced signal it is.

## Keys: one identity per entity, or the relations never meet

Here is the second-most-common trap after flattening. You describe the same
entity in two relations, under two **different keys**, and they silently refuse to
join.

In our tool, some facts are naturally **command-keyed** and others
**invocation-keyed**:

```text
relation config_file(sym, sym);    % config_file(command,    path) — a command reads this file
relation creates_file(sym, sym);   % creates_file(invocation, path) — this argv line created this file
```

Now ask: *which commands create files they also read as config?* You can't —
directly. `config_file` is keyed on the command (`"add"`); `creates_file` is keyed
on the whole argument line (`"add buy milk"`). They are about the same command,
but they live in **two different key-spaces**, and a join on equal symbols finds
nothing because `"add" ≠ "add buy milk"`.

The fix is the bridge relation you already built in Part 2. `uses_command` maps an
invocation to its command, and that is exactly the translator between the two
key-spaces:

```rust
use ascent_jit::{Engine, Value};
use fact_query::{Cardinality, FactStore};

let mut engine = Engine::from_source("
    relation arg(sym, int, sym);
    relation config_file(sym, sym);
    relation creates_file(sym, sym);
    relation uses_command(sym, sym);
    relation command_creates_config(sym, sym);   // command, path
    uses_command(argv, cmd) <-- arg(argv, 0, cmd);
    command_creates_config(cmd, path) <--
        creates_file(argv, path),
        uses_command(argv, cmd),
        config_file(cmd, path);
").unwrap();

// `add buy milk` creates the very file the `add` command reads as config.
let intern = |e: &mut Engine, s: &str| Value::Sym(e.intern(s));
let (argv, cmd, path) = ("add buy milk", "add", "todos.json");
let a = intern(&mut engine, argv);
let c = intern(&mut engine, cmd);
engine.add_fact("arg", vec![a, Value::Int(0), c]).unwrap();
let (a, p) = (intern(&mut engine, argv), intern(&mut engine, path));
engine.add_fact("creates_file", vec![a, p]).unwrap();
let (c, p) = (intern(&mut engine, cmd), intern(&mut engine, path));
engine.add_fact("config_file", vec![c, p]).unwrap();
engine.run().unwrap();

let q = engine.parse_query("(cmd, p) <-- command_creates_config(cmd, p)").unwrap();
engine.check(&q).unwrap();
let (results, _) = engine.eval(&q, Cardinality::new(1_000)).unwrap();
assert_eq!(results.rows().len(), 1); // the bridge let the two key-spaces meet
```

The lessons:

- **Pick one identity per entity** and use it consistently. An invocation is keyed
  by its argument line; a command by its name. Don't key "the same thing" two ways.
- When two relations legitimately describe one entity at **different grains**
  (per-command vs. per-invocation), don't force one to change — build the
  **bridge relation** that relates the grains, and join through it. That bridge is
  almost always a projection you can *derive* (here, `arg(argv, 0, cmd)`), which is
  another reason Part 2's decomposition pays off.

## Grounding: doc-strings are load-bearing

A relation named `arg` with columns `(sym, int, sym)` is meaningless to anyone —
human or LLM — who didn't write it. Is the `int` the position or the exit code? Is
the first `sym` the invocation or the token? Whoever proposes a query has to
*know* the schema's meaning, not just its arity. So **document every relation**:

```text
relation arg(sym, int, sym);   % arg(invocation-id, 0-based position, token)
```

[The substrate treats doc-strings as part of the schema contract](../fact-query/README.md)
precisely because a fact base of cryptic names is close to ungroundable: a
proposer can't write a correct query over relations it can't interpret. The doc is
not decoration; it is the interface.

## Provenance: an audit of the model, not just the answer

Part 1 introduced provenance as "which facts produced this row." Its second, less
obvious use is as a **check on your modeling**. When you believe two facts are
related but a derivation over them comes back empty, or provenance can't cite the
connection you expected, that absence is a signal: **the relationship isn't in the
model.** That is exactly the diagnosis for the flattened argv in Part 2 — provenance
couldn't link an invocation to its command because no relation expressed the link.

So when results surprise you, ask provenance *why*, and read its silence as
carefully as its citations. A missing justification usually means a missing
relation — and the fix is upstream, in how you lowered the data, not in the query.

## The modeling checklist

When you sit down to model something, run it through this:

1. **Lower to `{Int | Bool | Sym}`.** Numbers → `Int`, flags → `Bool`, everything
   else (names, ids, paths, hashes, enum tags) → interned `Sym`. No new types.
2. **Did I flatten a structure into one `Sym`?** If I'd ever want to split,
   index, or prefix-test it in a rule, yes — decompose at ingest (Part 2).
3. **Fixed shape → columns. Variable-length → side relation.** Add an index
   column only if order or duplicates matter.
4. **One identity per entity.** If two relations describe the same thing at
   different grains, build the bridge relation that joins them (Part 3).
5. **Document every relation** — what each column *means*, not just its type.
6. **Bound holds.** The interesting answers fit under the cardinality cap, and a
   truncation is handled as "unknown / fail-closed," never as "empty."
7. **Audit with provenance.** If provenance can't cite a link you expected, the
   model is missing a relation — fix it upstream.

---

**Part 3 in one line:** index a collection only when order matters, aggregate to
summarize it within the bound, keep one key per entity and bridge differing
grains, and lean on doc-strings and provenance to keep the lowering honest.
</content>
