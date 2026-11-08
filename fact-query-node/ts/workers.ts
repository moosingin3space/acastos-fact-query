/**
 * Cloudflare Workers entry point for `@acastos/fact-query`.
 *
 * Same API as the Node entry point (see `index.ts`); wrangler selects this one
 * through the package's `workerd` export condition. Workers import a `.wasm`
 * file as an already-compiled `WebAssembly.Module` and do not let code compile
 * WebAssembly from bytes, so this entry imports the module and instantiates it
 * synchronously with wasm-bindgen's `initSync`, once per isolate, at module
 * scope.
 *
 * @example
 * ```ts
 * import { FactEngine } from "@acastos/fact-query";
 *
 * export default {
 *   async fetch(): Promise<Response> {
 *     const engine = FactEngine.fromSource("relation edge(int, int);");
 *     engine.addFacts([{ relation: "edge", values: [1n, 2n] }]);
 *     engine.run();
 *     const { rows } = engine.query("(x, y) <-- edge(x, y)");
 *     engine.free();
 *     return Response.json(rows.map((row) => row.map(String)));
 *   },
 * };
 * ```
 *
 * @packageDocumentation
 */

import wasmModule from "../wasm-workers/fact_query_node_bg.wasm";
import { FactEngine as WasmFactEngine, initSync } from "../wasm-workers/fact_query_node.js";
import { setBinding } from "./core.js";

initSync({ module: wasmModule });
setBinding(WasmFactEngine);

export {
  DEFAULT_MAX_CARDINALITY,
  DEFAULT_MAX_JUSTIFICATIONS_PER_ROW,
  DEFAULT_MAX_ROWS,
  FactEngine,
  FactQueryError,
} from "./core.js";
export type {
  ColumnType,
  FactValue,
  FactValueInput,
  Provenance,
  QueryLimits,
  QueryResult,
  RelationSchema,
  Schema,
  Stage,
  SupportFact,
} from "./core.js";
