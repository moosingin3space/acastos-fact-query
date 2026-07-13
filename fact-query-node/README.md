# @acastos/fact-query

Node.js / TypeScript bindings for
[`fact-query`](../fact-query) — a **governance-free** proposer/verifier
substrate over an [`ascent-jit`](../ascent-jit) Datalog fact base.

An untrusted query is proposed; the substrate **parses** it, **form-checks** it
(schema-validity and safety / range-restriction), and **evaluates** it read-only
under a cardinality bound, returning the rows **with provenance**. Some net you
build — a human, a vote, or nothing — decides whether that result is what was
wanted. It checks *form*, not *meaning*: a query can parse, type-check, run, and
return a clean result that does not answer the question asked. **Provenance is
the one bridge** the substrate offers against that.

Evaluation runs entirely in **WebAssembly** (the engine's expression tier on the
host's own `WebAssembly` engine), so there is no native `wasmtime` dependency.
Nothing here carries policy, an LLM, or a commit path — that is the
governance-free guarantee, layered on *top* by your application.

## Install

Until published to npm, install from git:

```sh
npm install github:moosingin3space/acastos-fact-query#main
```

Or add to `package.json`:

```json
{
  "dependencies": {
    "@acastos/fact-query": "github:moosingin3space/acastos-fact-query#main"
  }
}
```

After running the Cargo build locally, you can also use a file path:

```json
{
  "dependencies": {
    "@acastos/fact-query": "file:../path/to/fact-query-node"
  }
}
```

## Use

```ts
import { FactEngine } from "@acastos/fact-query";

// 1. Load a program (relations + optional rules) as data.
const engine = FactEngine.fromSource(
  `relation edge(int, int);
   relation path(int, int);
   path(x, y) <-- edge(x, y);
   path(x, z) <-- edge(x, y), path(y, z);`,
);

// 2. Ingest facts and run to a fixed point.
engine.addFacts([
  { relation: "edge", values: [1n, 2n] },
  { relation: "edge", values: [2n, 3n] },
]);
engine.run();

// 3. Parse -> form-check -> bounded, read-only evaluation, with provenance.
const { rows, truncated, provenance } = engine.query(
  "(x, z) <-- path(x, z)",
);

console.log(rows);       // [[1n, 2n], [2n, 3n], [1n, 3n]]
console.log(truncated);  // false
// provenance[i] explains rows[i]: the body facts that joined to yield it.
```

### Grounding an LLM proposer

`schema()` returns the relations, their column types, and (where the backend has
them) doc-strings — the material a proposer needs to write a query that means
what was asked:

```ts
const schema = engine.schema();
// { relations: [{ name: "edge", columns: ["int","int"], doc: null }, ...] }
```

This binding was built to drop a deterministic, provenance-carrying verifier
underneath an LLM loop (e.g. the OpenRouter Agents SDK): the model proposes a
query, you `check()` it cheaply, `query()` it under a bound, and show the
`provenance` as the evidence a human or a vote signs off on.

### Choosing the expression evaluator

`fromSource` takes an optional second argument selecting how `if`/`let`/head
expressions run — the relational core and all semantics are identical either
way (the interpreter is the differential oracle the wasm path is pinned to):

```ts
const engine = FactEngine.fromSource(src, "interpreted");
```

- `"wasm"` (default) — a nested `WebAssembly` module via the host engine.
  Every expression call crosses the JS boundary.
- `"interpreted"` — a pure tree-walk, in-substrate, zero boundary crossings.
  Measured faster under Node on every benchmarked workload (up to ~13× on
  expression-heavy programs; see `npm run bench` and
  [`docs/0008`](../docs/0008-benchmarks-and-the-join-jit-deferral.md)).

## The value model

Every column is one of three kinds, mapped to disjoint JS types so a value is
self-describing:

| Engine | Out (`FactValue`) | In (`FactValueInput`)         |
| ------ | ----------------- | ----------------------------- |
| `int`  | `bigint`          | `bigint` or integer `number`  |
| `bool` | `boolean`         | `boolean`                     |
| `sym`  | `string`          | `string`                      |

Integers are `i64`; results are always `bigint` (a JS `number` cannot hold the
full range). On input a `number` is accepted but must be an integer — a
non-integer throws a `TypeError`. Symbols cross as their **strings**, never raw
interner ids.

## Errors and the contract

Every rejection throws a `FactQueryError` whose `stage` says which of the
contract's guarantees failed:

| `stage`  | Meaning                                                          |
| -------- | --------------------------------------------------------------- |
| `parse`  | The text is not a valid query.                                  |
| `schema` | A referenced relation / arity / column type does not exist.     |
| `unsafe` | The query is not range-restricted (or uses a disallowed feature, e.g. negation). |
| `eval`   | Evaluation faulted — **indeterminate**, treat fail-closed.      |
| `engine` | Building the engine, ingesting a fact, or running failed.       |

`truncated === true` means evaluation stopped at the cardinality cap and the
rows are a **prefix** — a first-class outcome, not an error. A decision keyed on
"the result is empty" must distinguish this case. An `eval` fault **throws**
(never returns an empty result), so a safety-conscious caller fails closed by
construction.

## Build and develop locally

When installing via git or file path, you must build the WebAssembly and
TypeScript. Requires the Rust `wasm32-unknown-unknown` target and the matching
`wasm-bindgen` CLI:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.125   # match the pinned crate
cd fact-query-node
npm install
npm run build      # cargo build (wasm) -> wasm-bindgen -> tsc
npm test
```

In the workspace root, `just node-build` and `just node-test` run the same
recipes.

## Design

The substrate's contract and seams are recorded in
[`../docs`](../docs); this binding is
[`docs/0006`](../docs/0006-typescript-node-binding.md).
