"use strict";

/**
 * Benchmark: the two expression evaluators exposed by `FactEngine.fromSource`.
 *
 *   - "wasm"        — expressions run on the host's WebAssembly engine via the
 *                     nested WebExecutor (substrate-wasm -> JS -> inner wasm).
 *   - "interpreted" — expressions run in-substrate by the pure tree-walk
 *                     interpreter, with zero JS-boundary crossings.
 *
 * Hypothesis under test (ADR 0006): in the Node deployment the whole substrate
 * is already wasm, so the nested-wasm expression tier pays a boundary cost per
 * expression call that the in-substrate interpreter avoids. We expect the
 * interpreter to win on expression-heavy work and to roughly tie on work with
 * no expressions (pure joins).
 *
 * Methodology: warmup runs, then >=10 timed iterations per case with
 * performance.now(); report median and spread (min/max). A fresh engine is
 * built per iteration for run()-based cases (run() mutates state to a fixed
 * point); the read-only query() case reuses one materialized engine.
 */

const { FactEngine } = require("../dist/index.js");

const WARMUP = 3;
const ITERS = 12;
const EVALUATORS = ["wasm", "interpreted"];

/** Median of a numeric array (linear interpolation for even length). */
function median(xs) {
  const s = [...xs].sort((a, b) => a - b);
  const mid = s.length / 2;
  return s.length % 2 ? s[Math.floor(mid)] : (s[mid - 1] + s[mid]) / 2;
}

/** Summary stats over a sample of millisecond timings. */
function stats(samples) {
  return {
    median: median(samples),
    min: Math.min(...samples),
    max: Math.max(...samples),
  };
}

/**
 * Times `body()` over ITERS iterations after WARMUP untimed ones. `setup()`
 * runs before each iteration and its result is passed to `body`; only `body` is
 * timed. `teardown(state)` (optional) runs after each iteration, untimed.
 */
function measure(setup, body, teardown) {
  for (let i = 0; i < WARMUP; i++) {
    const state = setup();
    body(state);
    if (teardown) teardown(state);
  }
  const samples = [];
  for (let i = 0; i < ITERS; i++) {
    const state = setup();
    const t0 = performance.now();
    body(state);
    const t1 = performance.now();
    if (teardown) teardown(state);
    samples.push(t1 - t0);
  }
  return stats(samples);
}

// --- Workload (a): transitive closure run() over a chain graph ---------------

const TC_SRC =
  "relation edge(int, int);" +
  "relation path(int, int);" +
  "path(x, y) <-- edge(x, y);" +
  "path(x, z) <-- edge(x, y), path(y, z);";

/** Chain edges 0->1->...->(n-1). */
function chainEdges(n) {
  const facts = [];
  for (let i = 0; i < n - 1; i++) {
    facts.push({ relation: "edge", values: [i, i + 1] });
  }
  return facts;
}

function benchTransitiveClosure(evaluator, n) {
  const facts = chainEdges(n);
  return measure(
    () => {
      const e = FactEngine.fromSource(TC_SRC, evaluator);
      e.addFacts(facts);
      return e;
    },
    (e) => e.run(),
    (e) => e.free(),
  );
}

// --- Workload (b): expression-heavy run() (chained if + let, minimal join) ---

// One relation, no join; each fact fires a rule with a chain of `if` filters
// and `let` arithmetic, so the expression tier dominates. Every `if` and `let`
// is a separate expression the tier evaluates (a boundary crossing under wasm).
const EXPR_SRC =
  "relation num(int);" +
  "relation derived(int, int);" +
  "derived(x, y) <-- num(x), " +
  "if x % 2 == 0, " +
  "if x % 7 != 0, " +
  "let a = x * x + 3, " +
  "let b = a * 2 - x, " +
  "let c = b % 1000, " +
  "let d = c * c + a, " +
  "let y = d % 100003;";

const EXPR_N = 400;

function exprFacts(n) {
  const facts = [];
  for (let i = 0; i < n; i++) {
    facts.push({ relation: "num", values: [i] });
  }
  return facts;
}

function benchExpressionHeavy(evaluator, n) {
  const facts = exprFacts(n);
  return measure(
    () => {
      const e = FactEngine.fromSource(EXPR_SRC, evaluator);
      e.addFacts(facts);
      return e;
    },
    (e) => e.run(),
    (e) => e.free(),
  );
}

// --- Workload (c): query() over a materialized base (2-atom join + cond) -----

const QUERY_SRC = "relation link(int, int);relation tag(int, int);";
// N sources fan into D mids; each mid fans out to K targets. The join on the
// mid produces up to N*K candidate rows, each tested by `if i < j`.
const Q_N = 300;
const Q_D = 30;
const Q_K = 40;
const QUERY_TEXT = "(i, j) <-- link(i, m), tag(m, j), if i < j";
const QUERY_CAP = 1_000_000;

function buildQueryEngine(evaluator) {
  const e = FactEngine.fromSource(QUERY_SRC, evaluator);
  const link = [];
  for (let i = 0; i < Q_N; i++) {
    link.push({ relation: "link", values: [i, i % Q_D] });
  }
  const tag = [];
  for (let m = 0; m < Q_D; m++) {
    for (let j = 0; j < Q_K; j++) {
      tag.push({ relation: "tag", values: [m, j] });
    }
  }
  e.addFacts(link);
  e.addFacts(tag);
  e.run();
  return e;
}

function benchQuery(evaluator) {
  const engine = buildQueryEngine(evaluator);
  const result = engine.query(QUERY_TEXT, QUERY_CAP);
  const rowCount = result.rows.length;
  const truncated = result.truncated;
  // query() is read-only; reuse the one materialized engine across iterations.
  const s = measure(
    () => engine,
    (e) => e.query(QUERY_TEXT, QUERY_CAP),
    null,
  );
  engine.free();
  return { stats: s, rowCount, truncated };
}

// --- Runner ------------------------------------------------------------------

function fmt(ms) {
  return ms.toFixed(3).padStart(9);
}

function reportPair(label, byEval) {
  const w = byEval.wasm;
  const i = byEval.interpreted;
  const ratio = i.median / w.median;
  console.log(
    `  ${label.padEnd(34)} ` +
      `wasm ${fmt(w.median)}ms [${fmt(w.min)}, ${fmt(w.max)}]   ` +
      `interp ${fmt(i.median)}ms [${fmt(i.min)}, ${fmt(i.max)}]   ` +
      `interp/wasm ${ratio.toFixed(3)}`,
  );
  return { label, wasm: w.median, interpreted: i.median, ratio };
}

function main() {
  console.log(
    `fact-query-node evaluator benchmark  (warmup=${WARMUP}, iters=${ITERS})\n` +
      `node ${process.version}\n`,
  );
  const summary = [];

  console.log("(a) transitive closure run() over a chain graph");
  for (const n of [8, 16, 32]) {
    const byEval = {};
    for (const ev of EVALUATORS) byEval[ev] = benchTransitiveClosure(ev, n);
    summary.push(reportPair(`chain n=${n}`, byEval));
  }

  console.log("\n(b) expression-heavy run() (chained if + let arithmetic)");
  {
    const byEval = {};
    for (const ev of EVALUATORS) byEval[ev] = benchExpressionHeavy(ev, EXPR_N);
    summary.push(reportPair(`num facts=${EXPR_N}, 2 if + 5 let`, byEval));
  }

  console.log("\n(c) query() over a materialized base (2-atom join + if)");
  {
    const byEval = {};
    let meta = null;
    for (const ev of EVALUATORS) {
      const r = benchQuery(ev);
      byEval[ev] = r.stats;
      meta = r;
    }
    summary.push(
      reportPair(
        `join rows=${meta.rowCount}${meta.truncated ? " (truncated)" : ""}`,
        byEval,
      ),
    );
  }

  console.log("\n=== summary: median ms, interpreted/wasm ratio ===");
  console.log(
    "workload".padEnd(38) +
      "wasm(ms)".padStart(12) +
      "interp(ms)".padStart(12) +
      "ratio".padStart(10),
  );
  for (const r of summary) {
    console.log(
      r.label.padEnd(38) +
        r.wasm.toFixed(3).padStart(12) +
        r.interpreted.toFixed(3).padStart(12) +
        r.ratio.toFixed(3).padStart(10),
    );
  }
}

main();
