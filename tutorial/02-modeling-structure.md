# Part 2 — Don't flatten structure into a symbol

This is the heart of the guide. Almost every modeling mistake in Acastos is the
same mistake: you have structured data — a list, a pair, a record — and the value
model has no type for it, so you flatten it into a single `Sym` and move on. The
data is now *stored* but no longer *reasoned about*. This part shows the trap with
a concrete example, names the smell that tells you you're in it, and gives the
two fixes.

## The trap: an argument vector as one symbol

We're characterizing a `todo` command-line tool. An invocation's argument line is
the natural key, so we record it:

```rust
use ascent_jit::{Engine, Value};

let mut engine = Engine::from_source("relation invocation(sym, int);").unwrap();
for (line, code) in [("add buy milk", 0), ("add first todo", 0), ("check 1", 0), ("check 999", 1)] {
    let argv = Value::Sym(engine.intern(line));
    engine.add_fact("invocation", vec![argv, Value::Int(code)]).unwrap();
}
```

Read `"add buy milk"` again. That is not a string — it is a **list**:
`["add", "buy", "milk"]`, a command followed by its arguments. We flattened a
sequence into one `Sym`. It looks harmless. It is not.

Now try to answer a question any reasoning system should handle:

> *Which command does each invocation use?*

The command is the **first token** of the argument vector. But the vector is now
an opaque symbol, and — recall Part 1 — **you cannot look inside a `Sym`**. There
is no `split`, no `first`, no prefix test available in a rule, by design: the
expression tier is closed arithmetic and comparison over `{Int | Bool | Sym}`,
and a symbol is identity, not text. The token you need is right there in front of
you and the engine cannot reach it.

So the query you want — "join each invocation to its command" — **cannot be
written at all.** The structure you flattened away is exactly the structure you
needed.

## The smell: you start re-asserting the parts by hand

Faced with that wall, everyone reaches for the same workaround. They add a second
relation and re-state the command manually:

```text
command("add").     % typed out by a human, or by a script that *also* had to tokenize
command("check").
```

Stop the moment you do this. **Re-asserting a piece of a value you already have
is the smell of a flattened structure.** Notice what you've actually got:

- `invocation("add buy milk", 0)` — the command `add` is in here, inert.
- `command("add")` — the command `add`, again, as a separate hand-entered fact.
- **Nothing connects them.** There is no rule deriving that this invocation uses
  that command, because the only thing that could — splitting the argv — doesn't
  exist. If you ask provenance why some downstream conclusion holds, it cannot
  cite a link between the invocation and its command, because **there is no link
  in the model.** Provenance can't explain a relationship you never represented.

The data redundantly contains the answer in two places and still cannot join them.
That is the cost of flattening, made concrete.

## Fix A — a fixed-shape composite becomes multiple columns

The simplest case first. When a value is a **fixed-arity** composite — a pair, a
triple, a known record — give each component its own column. Don't pack a
`(command, rest-of-args)` pair into one symbol; lower it to columns the moment it
enters:

```rust
use ascent_jit::{Engine, Value};
use fact_query::{Cardinality, FactStore};

// command, argument-string, exit-code — each a column.
let mut engine = Engine::from_source("relation invocation(sym, sym, int);").unwrap();

// Tokenize at ingest — where you DO have a real splitter — not at query time.
for (cmd, rest, code) in [("add", "buy milk", 0), ("check", "999", 1)] {
    let c = Value::Sym(engine.intern(cmd));
    let r = Value::Sym(engine.intern(rest));
    engine.add_fact("invocation", vec![c, r, Value::Int(code)]).unwrap();
}
engine.run().unwrap();

// The command is now a first-class column, so the join is trivial — and you
// never needed the hand-entered `command` table, because it is derivable.
let q = engine.parse_query("(cmd) <-- invocation(cmd, _, code), if code != 0").unwrap();
engine.check(&q).unwrap();
let (results, _) = engine.eval(&q, Cardinality::new(1_000)).unwrap();
assert_eq!(results.rows(), &[vec![Value::Sym(engine.intern("check"))]]);
```

This is exactly the rule [docs/0003](../docs/0003-external-fact-sources.md) gives
for external data: *a composite identity lowers to multiple columns so rules can
join on the parts, rather than to one opaque blob.* The blob blocks the join; the
columns enable it.

## Fix B — a variable-length ordered sequence becomes an indexed side relation

Fix A assumes you know the arity. An argument vector doesn't have one: `list` has
one token, `rename 2 new task name` has five. For a **variable-length, ordered**
sequence, the right shape is a **side relation with an index column** — one row
per element, carrying its position:

```text
relation arg(sym, int, sym);   % arg(invocation-id, position, token)
```

The full argument line stays as the **identity** that ties the rows together (and
joins to `invocation`, `output_contains`, etc.); the `arg` relation carries its
*contents*, positioned. Now everything the flattened form refused is a one-line
rule — including the command, which is just position 0:

```rust
use ascent_jit::{Engine, Value};
use fact_query::{Cardinality, FactStore};

let mut engine = Engine::from_source("
    relation arg(sym, int, sym);
    relation uses_command(sym, sym);            // invocation, command
    uses_command(argv, cmd) <-- arg(argv, 0, cmd);
").unwrap();

// "rename 2 new task name" lowers to one fact per token, indexed by position.
let line = "rename 2 new task name";
for (i, tok) in ["rename", "2", "new", "task", "name"].iter().enumerate() {
    let id = Value::Sym(engine.intern(line));
    let t = Value::Sym(engine.intern(tok));
    engine.add_fact("arg", vec![id, Value::Int(i as i64), t]).unwrap();
}
engine.run().unwrap();

// Recover the join the flattened model couldn't express. And now provenance
// *can* cite the arg(argv, 0, cmd) fact as the reason: the relationship exists.
let q = engine.parse_query("(argv, cmd) <-- uses_command(argv, cmd)").unwrap();
engine.check(&q).unwrap();
let (results, _) = engine.eval(&q, Cardinality::new(1_000)).unwrap();
assert_eq!(
    results.rows(),
    &[vec![Value::Sym(engine.intern(line)), Value::Sym(engine.intern("rename"))]],
);
```

The literal `0` in `arg(argv, 0, cmd)` is a real, supported atom argument (the
grammar allows integer, string, and boolean literals in body positions), so
"the token at position 0" is something a rule can actually say.

## The rule of thumb

> **If you ever wish you could split, index into, prefix-test, or pattern-match a
> `Sym` inside a rule, you have flattened a structure. Decompose it at ingest
> instead — into columns if its shape is fixed, into an indexed side relation if
> its length varies.**

Ingest is where you have a real programming language, a real tokenizer, a real
parser. Query time is not — and shouldn't be; pushing string-surgery into the
rule engine is precisely the complexity the closed value model refuses to take
on. Do the structural work once, on the way in, and leave the engine to do what
it is good at: joining identities and deriving over them.

[Part 3](03-collections-keys-grounding.md) sharpens the side-relation idea — when
the index column earns its place and when it's clutter — and covers the next trap:
modeling the same entity under two different keys so the relations can never meet.

---

**Part 2 in one line:** never hide structure in a `Sym`; lower a fixed composite
to columns and a variable-length ordered sequence to an indexed side relation,
and do it at ingest where you still have the parts.
</content>
