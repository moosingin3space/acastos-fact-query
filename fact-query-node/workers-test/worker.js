// A Worker that exercises the Workers build inside workerd. It imports the
// built entry by path (Miniflare loads modules unbundled), so this checks the
// runtime half; wrangler's `workerd` export-condition resolution is the
// bundler half.
import { FactEngine, FactQueryError } from "../dist-workers/workers.js";

const SRC =
  "relation edge(int, int);" +
  "relation path(int, int);" +
  "path(x, y) <-- edge(x, y);" +
  "path(x, z) <-- edge(x, y), path(y, z);";

/** BigInts do not survive JSON; render every value as a string. */
const text = (rows) => rows.map((row) => row.map(String));

export default {
  async fetch() {
    const engine = FactEngine.fromSource(SRC);
    try {
      engine.addFacts([
        { relation: "edge", values: [1n, 2n] },
        { relation: "edge", values: [2n, 3n] },
      ]);
      engine.run();
      const result = engine.query("(x, z) <-- path(x, z), if x < z");

      let stage = null;
      try {
        engine.query("(x) <-- nope(x)");
      } catch (error) {
        stage = error instanceof FactQueryError ? error.stage : `not a FactQueryError: ${error}`;
      }

      return Response.json({
        rows: text(result.rows).sort(),
        truncated: result.truncated,
        support: result.rows.map((_, r) => result.provenance.justifications(r).length),
        atoms: result.provenance.atoms,
        evidenceTruncated: result.provenance.truncated,
        schema: engine.schema().relations.map((r) => r.name),
        errorStage: stage,
      });
    } finally {
      engine.free();
    }
  },
};
