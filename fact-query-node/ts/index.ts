/**
 * Node.js / TypeScript bindings for the `fact-query` queries grain.
 *
 * An untrusted query is proposed; this substrate parses it, form-checks it
 * (schema-validity and safety / range-restriction), and evaluates it read-only
 * under a cardinality bound, returning the rows **with provenance** — and some
 * net you build (show-the-evidence, a paraphrase round-trip, an N-candidate
 * vote) decides whether the result is what was wanted. It checks *form*, not
 * *meaning*: a query can parse, type-check, run, and return a clean result that
 * does not answer the question asked. Provenance is the one bridge offered.
 *
 * Evaluation runs entirely in WebAssembly; nothing here carries policy, an LLM,
 * or a commit path.
 *
 * @example
 * ```ts
 * import { FactEngine } from "@acastos/fact-query";
 *
 * const engine = FactEngine.fromSource("relation edge(int, int);");
 * engine.addFacts([
 *   { relation: "edge", values: [1n, 2n] },
 *   { relation: "edge", values: [2n, 3n] },
 * ]);
 * engine.run();
 *
 * const { rows, truncated, provenance } = engine.query(
 *   "(x, z) <-- edge(x, y), edge(y, z)",
 * );
 * // rows === [[1n, 3n]], truncated === false
 * // provenance[0].justifications[0].support has the two edges that joined.
 * ```
 *
 * @packageDocumentation
 */

import { FactEngine as WasmFactEngine } from "../wasm/fact_query_node.js";
import { setBinding } from "./core.js";

setBinding(WasmFactEngine);

export {
  DEFAULT_MAX_CARDINALITY,
  FactEngine,
  FactQueryError,
} from "./core.js";
export type {
  ColumnType,
  FactValue,
  FactValueInput,
  Justification,
  QueryResult,
  RelationSchema,
  RowProvenance,
  Schema,
  Stage,
  SupportTuple,
} from "./core.js";
