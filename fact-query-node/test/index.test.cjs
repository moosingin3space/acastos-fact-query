"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");

const { FactEngine, FactQueryError } = require("../dist/index.js");

/** Runs `fn`, returning the error it throws (failing if it throws nothing). */
function capture(fn) {
  try {
    fn();
  } catch (error) {
    return error;
  }
  assert.fail("expected the call to throw, but it did not");
}

test("transitive join returns rows with provenance", () => {
  const engine = FactEngine.fromSource("relation edge(int, int);");
  engine.addFacts([
    { relation: "edge", values: [1n, 2n] },
    { relation: "edge", values: [2n, 3n] },
  ]);
  engine.run();

  const { rows, truncated, provenance } = engine.query(
    "(x, z) <-- edge(x, y), edge(y, z)",
  );

  assert.deepEqual(rows, [[1n, 3n]]);
  assert.equal(truncated, false);
  assert.equal(provenance.truncated, false);
  assert.deepEqual(provenance.atoms, ["edge", "edge"]);
  // The two edges that joined to yield the row, in body order.
  assert.deepEqual(provenance.justifications(0), [
    [
      { relation: "edge", tuple: [1n, 2n] },
      { relation: "edge", tuple: [2n, 3n] },
    ],
  ]);
  assert.deepEqual(provenance.justifications(1), [], "out of range");
  engine.free();
});

test("provenance stores each supporting fact once", () => {
  const engine = FactEngine.fromSource("relation edge(int, int);");
  engine.addFacts([
    { relation: "edge", values: [1n, 2n] },
    { relation: "edge", values: [2n, 3n] },
    { relation: "edge", values: [2n, 4n] },
  ]);
  engine.run();
  // Both 2-hop paths go through edge(1, 2).
  const { rows, provenance } = engine.query("(x, z) <-- edge(x, y), edge(y, z)");
  assert.equal(rows.length, 2);
  assert.equal(provenance.facts.length, 3);
  const [first] = provenance.justifications(0);
  const [second] = provenance.justifications(1);
  assert.equal(first[0], second[0], "the shared edge is the same object");
  engine.free();
});

test("number and bigint inputs are equivalent integers", () => {
  const engine = FactEngine.fromSource("relation edge(int, int);");
  engine.addFact("edge", [1, 2]); // plain numbers
  engine.run();
  const { rows } = engine.query("(a, b) <-- edge(a, b)");
  assert.deepEqual(rows, [[1n, 2n]]);
});

test("integers beyond MAX_SAFE_INTEGER round-trip losslessly", () => {
  const big = 9007199254740993n; // 2^53 + 1
  const engine = FactEngine.fromSource("relation big(int);");
  engine.addFact("big", [big]);
  engine.run();
  const { rows } = engine.query("(n) <-- big(n)");
  assert.deepEqual(rows, [[big]]);
});

test("symbols cross as strings, not interner ids", () => {
  const engine = FactEngine.fromSource("relation owner(sym, sym);");
  engine.addFact("owner", ["alice", "repo"]);
  engine.run();
  const { rows } = engine.query("(p, r) <-- owner(p, r)");
  assert.deepEqual(rows, [["alice", "repo"]]);
});

test("truncation is a first-class outcome, not an error", () => {
  const engine = FactEngine.fromSource("relation edge(int, int);");
  engine.addFacts([
    { relation: "edge", values: [1n, 2n] },
    { relation: "edge", values: [1n, 3n] },
    { relation: "edge", values: [1n, 4n] },
  ]);
  engine.run();
  const { rows, truncated, provenance } = engine.query("(a, b) <-- edge(a, b)", 2);
  assert.equal(rows.length, 2);
  assert.equal(truncated, true);
  assert.equal(provenance.truncated, true, "evidence was not fully enumerated");
});

test("the row and evidence caps are independent", () => {
  const engine = FactEngine.fromSource("relation edge(int, int);");
  engine.addFacts([
    { relation: "edge", values: [1n, 2n] },
    { relation: "edge", values: [1n, 3n] },
    { relation: "edge", values: [2n, 3n] },
  ]);
  engine.run();
  const { rows, truncated, provenance } = engine.query("(a) <-- edge(a, _)", {
    maxJustificationsPerRow: 1,
  });
  assert.equal(rows.length, 2);
  assert.equal(truncated, false, "every row is present");
  assert.equal(provenance.truncated, true, "row (1) lost a justification");
  for (let r = 0; r < rows.length; r++) {
    assert.equal(provenance.justifications(r).length, 1);
  }
  assert.throws(() => engine.query("(a) <-- edge(a, _)", -1), TypeError);
  engine.free();
});

test("schema reports relations and column types", () => {
  const engine = FactEngine.fromSource(
    "relation edge(int, int); relation flag(bool); relation name(sym);",
  );
  const schema = engine.schema();
  const byName = Object.fromEntries(schema.relations.map((r) => [r.name, r]));
  assert.deepEqual(byName.edge.columns, ["int", "int"]);
  assert.deepEqual(byName.flag.columns, ["bool"]);
  assert.deepEqual(byName.name.columns, ["sym"]);
  assert.equal(byName.edge.doc, null);
});

test("a parse failure throws stage 'parse'", () => {
  const engine = FactEngine.fromSource("relation edge(int, int);");
  const err = capture(() => engine.query("this is not a query"));
  assert.ok(err instanceof FactQueryError);
  assert.equal(err.stage, "parse");
});

test("an unknown relation throws stage 'schema'", () => {
  const engine = FactEngine.fromSource("relation edge(int, int);");
  const err = capture(() => engine.check("(x) <-- nope(x)"));
  assert.ok(err instanceof FactQueryError);
  assert.equal(err.stage, "schema");
});

test("an unbound output variable throws stage 'unsafe'", () => {
  const engine = FactEngine.fromSource("relation edge(int, int);");
  // `z` is bound by no positive body literal.
  const err = capture(() => engine.check("(x, z) <-- edge(x, y)"));
  assert.ok(err instanceof FactQueryError);
  assert.equal(err.stage, "unsafe");
});

test("a non-integer number input throws a TypeError, not a FactQueryError", () => {
  const engine = FactEngine.fromSource("relation edge(int, int);");
  const err = capture(() => engine.addFact("edge", [1.5, 2]));
  assert.ok(err instanceof TypeError);
  assert.ok(!(err instanceof FactQueryError));
});
