# 0011: A Cloudflare Workers build of the npm package

- **Status:** Accepted
- **Crate:** `fact-query-node` (packaging only; no Rust changes)
- **Builds on:** [0006](0006-typescript-node-binding.md) (the binding),
  [0010](0010-retire-the-wasm-expression-tier.md) (one wasm module, no runtime
  compilation)

## Context

The npm package was built with `wasm-bindgen --target nodejs`. The generated
glue loads its wasm with `require('fs').readFileSync(...)`, which does not exist
in workerd. The runtime has two further constraints:

- Workers load WebAssembly only as modules uploaded with the Worker. Importing
  a `.wasm` file yields an already-compiled `WebAssembly.Module`, not an
  instance.
- Code generation at runtime is restricted. Before 0010 this mattered most:
  the old `"wasm"` evaluator compiled a module from bytes for the program's
  expressions. Now the interpreter is compiled into the one module, so nothing
  is ever compiled at runtime.

Cloudflare documents a route for raw `wasm-bindgen` output: build with
`--target bundler`, then patch the emitted JS to instantiate the imported
module by hand.

## Decision

1. **One package, two builds, selected by export condition.** `package.json`
   gains an `exports` map. The `workerd` condition, which wrangler resolves when
   bundling, points at `dist-workers/workers.js`; every other resolver gets the
   unchanged CommonJS Node build in `dist/`. `main`/`types` stay for older
   resolvers.
2. **`--target web` plus `initSync`, not `bundler` plus a patch.**
   wasm-bindgen's `--target web` glue exports
   `initSync({ module: WebAssembly.Module })`, which only calls
   `new WebAssembly.Instance`. The Workers entry imports the `.wasm` as a
   module and calls it once at module scope (once per isolate). The generated
   glue is used unmodified. Its async `init` (`fetch` /
   `import.meta.url`) is never called.
3. **A shared core with an injected binding.** The wrapper (value conversion,
   `FactQueryError`, `FactEngine`) moves to `ts/core.ts`, typed against an
   `InnerEngineClass` interface; the generated `FactEngine` declarations are
   identical across targets. `ts/index.ts` (Node) and `ts/workers.ts` (Workers)
   each load their glue and call `setBinding`. Both entries export exactly the
   public API the Node entry exported before; `setBinding` is not reachable
   through the `exports` map.
4. **ESM output inside a CommonJS package.** The Workers build is compiled by a
   second tsconfig (`module: es2022`, `moduleResolution: bundler`) into
   `dist-workers/`. `scripts/mark-esm.cjs` drops `{"type": "module"}` into
   `dist-workers/` and `wasm-workers/`, so every tool reads them as ESM without
   changing the package's own type.
5. **Tested in the real runtime.** `npm run test:workers` loads the built entry
   in workerd via Miniflare (a new dev dependency). It runs a fixed point and a
   query, and checks provenance and a `FactQueryError` stage. It is kept out of
   `npm test`, so the Node suite does not need the workerd binary.

## Verification

Beyond `test:workers`, the bundler half was checked once by hand, outside the
repo:

- `npm pack`ed the package and installed it in a scratch project.
- `wrangler deploy --dry-run` (wrangler 4.152.0) resolved the bare
  `@acastos/fact-query` import to the Workers entry, kept the `.wasm` as a
  separate module, and reported 90 KiB gzipped in total.
- Running that bundled output in workerd returned the expected
  transitive-closure rows.

## Consequences

- **Workers users need no configuration.** The same import works in Node and in
  a Worker, because wrangler's defaults already import `.wasm` as modules.
- **The `exports` map closes deep imports.** Something like
  `@acastos/fact-query/dist/index.js` no longer resolves; only the package root
  and `package.json` are exported. Pre-1.0, and nothing documented deep paths.
- **Two wasm-bindgen outputs ship** from the same compiled `.wasm`, so the
  unpacked tarball carries the 285 KB module twice (about 650 KB in total).
  A deployed Worker includes only one copy.
- **Not yet addressed.** Durable state still needs Durable Objects plus a way to
  rebuild an engine. Engine snapshots ([0007](0007-engine-snapshot-serialization.md))
  remain proposed. The package exposes the queries grain only, as in 0006.
