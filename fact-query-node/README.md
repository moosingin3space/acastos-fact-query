# @acastos/fact-query

Node.js, Cloudflare Workers, and TypeScript bindings for
[`fact-query`](../fact-query) — a **governance-free** proposer/verifier
substrate over an [`ascent-jit`](../ascent-jit) Datalog fact base.

An untrusted query is proposed; the substrate **parses** it, **form-checks** it
(schema-validity and safety / range-restriction), and **evaluates** it read-only
under a cardinality bound, returning the rows **with provenance**. Some net you
build — a human, a vote, or nothing — decides whether that result is what was
wanted. It checks *form*, not *meaning*: a query can parse, type-check, run, and
return a clean result that does not answer the question asked. **Provenance is
the one bridge** the substrate offers against that.

Evaluation runs entirely in **WebAssembly**: the engine — a pure interpreter —
is compiled into the package's wasm module, so there is no native dependency.
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
// provenance.justifications(i) explains rows[i]: each justification is the
// body facts that joined to yield it, one per atom in provenance.atoms.
```

Provenance is normalized: each distinct supporting fact appears once in
`provenance.facts`, and justifications refer to those same objects, so evidence
shared by many rows is shipped and converted once. The raw form
(`offsets` / `support` as `Uint32Array`s) is there too, for bulk processing.

Pass caps as `engine.query(text, { maxRows, maxJustificationsPerRow })`
(defaults 10 000 and 16), or a number as shorthand for `maxRows`.

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

## Cloudflare Workers

The same import works in a Worker: the package's `workerd` export condition,
which wrangler resolves when bundling, selects a Workers build with the same
API. No configuration is needed, because wrangler's defaults already import
`.wasm` files as modules.

```ts
import { FactEngine } from "@acastos/fact-query";

export default {
  async fetch(): Promise<Response> {
    const engine = FactEngine.fromSource("relation edge(int, int);");
    try {
      engine.addFacts([{ relation: "edge", values: [1n, 2n] }]);
      engine.run();
      const { rows } = engine.query("(x, y) <-- edge(x, y)");
      return Response.json(rows.map((row) => row.map(String))); // bigint is not JSON
    } finally {
      engine.free();
    }
  },
};
```

Things to know:

- **Instantiation.** Workers load WebAssembly only as modules uploaded with the
  Worker. The Workers build imports its `.wasm` and instantiates it once per
  isolate at module scope. The whole Worker is about 90 KiB gzipped.
- **State is per request.** An isolate is not durable, so build the engine
  (load facts, `run()`) per request, or hold one in a Durable Object and
  rebuild it from storage after eviction.
- **Free engines.** The 128 MB memory limit is per isolate and shared by
  concurrent requests, so call `free()` when a request is done with its engine.
- **CPU limits are the time bound.** The engine bounds result cardinality and
  iterations, not time. A request over the plan's CPU limit is terminated, so
  it still fails closed. Small fact bases fit the free plan's 10 ms.

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

`truncated === true` means the row cap was reached and the rows are a
**prefix** — a first-class outcome, not an error. A decision keyed on "the
result is empty" must distinguish this case. Separately,
`provenance.truncated === true` means some row may have more justifications
than were returned; the rows themselves are still complete unless `truncated`
is also set. An `eval` fault **throws**
(never returns an empty result), so a safety-conscious caller fails closed by
construction.

## Build and develop locally

When installing via git or file path, you must build the WebAssembly and
TypeScript. Requires the Rust `wasm32-unknown-unknown` target and the matching
`wasm-bindgen` CLI:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126   # match the pinned crate
cd fact-query-node
npm install
npm run build          # cargo build (wasm) -> wasm-bindgen x2 -> tsc x2
npm test               # Node build
npm run test:workers   # Workers build, inside workerd (via Miniflare)
```

The build emits two pairs of directories: `wasm/` + `dist/` (Node, CommonJS)
and `wasm-workers/` + `dist-workers/` (Workers, ESM). In the workspace root,
`just node-build`, `just node-test`, and `just node-test-workers` run the same
recipes.

## Design

The substrate's contract and seams are recorded in
[`../docs`](../docs); this binding is
[`docs/0006`](../docs/0006-typescript-node-binding.md).
