# 0006: A Node.js / TypeScript binding

- **Status:** Proposed
- **Crate:** `fact-query-node` (a `wasm32`-only consumer crate, like
  `ascent-jit-web`)
- **Builds on:** [0002](0002-fact-query-substrate.md) (the queries-grain
  contract), [0004](0004-pluggable-wasm-execution.md) (the pluggable WASM
  executor and the browser `WebExecutor`)

## Context

`fact-query` is a Rust substrate. The shape it captures — propose an untrusted
artifact, evaluate it deterministically under a bound, decide with the
provenance in hand — is the same shape an **LLM agent loop** wants under it: the
model proposes a query, a deterministic verifier checks form and runs it, and the
evidence is shown to whatever net decides. Those agent loops are overwhelmingly
written in **TypeScript on Node** (the OpenRouter Agents SDK, among others). To
serve them the substrate must be callable from JS without a Rust toolchain at the
call site.

There were two ways in.

- **A native N-API addon** (napi-rs): compile a crate to a `.node` binary,
  keeping the native `wasmtime` JIT. Fastest execution, but ships per-platform
  prebuilt binaries (a CI matrix) and a heavy native dependency.
- **WebAssembly** (wasm-bindgen): compile the substrate to `wasm32` and run it
  under the host's own `WebAssembly` engine — which Node provides. One portable
  artifact, no native toolchain for consumers.

## Decision drivers

- **Reuse the executor seam, do not fork it.** ADR 0004 already split the
  expression tier into a runtime-free *encoder* and a swappable *executor*, and
  `ascent-jit-web` already supplies a `WebExecutor` over the host `WebAssembly`
  engine. Node has that same engine. A wasm binding is then almost entirely
  *reuse*; a native binding would re-introduce the `wasmtime` weld ADR 0004 spent
  effort removing.
- **One portable artifact.** A single `.wasm` runs on every OS/arch a Node
  install supports, with no prebuild matrix and no native compiler at install.
- **Stay governance-free (0002).** The binding must not learn an application's
  denial/trust/origin vocabulary. The crate depends on `fact-query` and never the
  reverse, and on no application — the dependency direction is the guarantee,
  unchanged by crossing into JS.
- **Preserve the contract across the boundary.** The five guarantees and the
  fail-closed disclaimer (0002) must survive into JS, not be flattened into a
  generic "it threw".

## Decision

Ship **`fact-query-node`**, a `wasm32`-only crate (workspace-excluded, like
`ascent-jit-web` and `ascent-jit/fuzz`) that wasm-bindgen-exports a `FactEngine`
over `ascent_jit_web::engine_from_source`, plus a thin TypeScript wrapper. The
package is distributed as an npm tarball after building locally or as a git
dependency from the repository.

### What crosses the boundary

The engine value model is closed and `Copy` — `{ Int(i64) | Bool | Sym }` (a
crate invariant). It maps to **three disjoint JS types**, so a value is
self-describing with no tag:

| Engine   | JS out    | JS in (also accepts) |
| -------- | --------- | -------------------- |
| `Int(i64)` | `bigint` | integer `number`     |
| `Bool`     | `boolean`| —                    |
| `Sym`      | `string` | —                    |

Two decisions matter here:

- **Integers are `bigint`, carried on the wire as a decimal string.** The model
  is `i64`; a JS `number` is `f64` and cannot hold it losslessly. Crossing as a
  decimal string sidesteps every `BigInt`/serde corner and is exact. The
  ergonomic `bigint` shaping is done in the TS layer.
- **Symbols cross as their resolved strings, never interner ids.** A `Symbol` id
  is meaningful only relative to the engine's interner (a `source.rs` invariant);
  leaking it across the boundary would be a run-dependent, meaningless integer.
  Symbol columns are interned on the way in and resolved on the way out.

### Preserving the contract

- Every rejection throws an `Error` carrying a **`stage`** property — `parse` /
  `schema` / `unsafe` / `eval` / `engine` — so the five guarantees (0002) remain
  distinguishable in JS. The TS layer re-wraps these as a typed `FactQueryError`.
- **Truncation is data, not an error:** the result object carries `truncated`.
- **Eval faults throw**, never return an empty result, so the fail-closed
  contract holds by construction — a `catch` that swallows the error is the only
  way to break it, and that is the caller's choice, surfaced.
- The binding adds **no new grain and no policy**. It exposes exactly the queries
  grain (`fromSource`, `addFact(s)`, `run`, `schema`, `check`, `query`) plus the
  ingestion needed to populate the base. `speculate`/`explain` and the facts /
  rules grains are deferred, as on the Rust side.

## Consequences

- **Reuse, not a second runtime.** Execution semantics are pinned to the same
  encoded bytes and the same `WebExecutor` the browser uses; the pure
  interpreter remains the differential oracle (0001/0004). Node and the browser
  run identical evaluation.
- **wasm-in-wasm cost.** Under Node the whole substrate is wasm, and the
  expression tier nests a second `WebAssembly` instantiation, with `i64`s
  crossing as `BigInt`. This is slower than the native `wasmtime` JIT. It is the
  price of one portable artifact and full executor reuse; a native napi backend
  remains a future option behind the same TS surface if profiling demands it.
- **A second build pipeline.** The crate is built with `cargo build --target
  wasm32-unknown-unknown` + `wasm-bindgen --target nodejs` + `tsc`, driven by
  npm scripts (and mirrored as `just node-build` / `just node-test`). The
  `wasm-bindgen` crate is pinned exact so the generated glue matches the CLI.
- **Grounding gap, unchanged.** `schema()` reports `doc: null` because the
  `ascent-jit` IR carries no relation doc-strings yet (0002). The binding
  surfaces the gap honestly rather than hiding it.

## Not in this ADR

A native (napi-rs) backend, browser/ESM build targets beyond `--target nodejs`,
and exposing `speculate`/`explain` are all deferred to when a consumer earns
them — the same discipline `fact-query` applies to its grains.
