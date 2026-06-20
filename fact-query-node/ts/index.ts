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

/** A column's runtime type, as reported by {@link FactEngine.schema}. */
export type ColumnType = "int" | "bool" | "sym";

/**
 * A column value as it leaves the engine. The three JS types are disjoint, so a
 * value is self-describing: a `bigint` is an integer (the engine's model is
 * `i64`, which `number` cannot hold losslessly), a `boolean` is a boolean, and a
 * `string` is a symbol.
 */
export type FactValue = bigint | boolean | string;

/**
 * A column value accepted as input. As {@link FactValue}, but a `number` is also
 * accepted for an integer (it must be an integer, else a `TypeError` is thrown;
 * for values beyond `Number.MAX_SAFE_INTEGER` pass a `bigint`).
 */
export type FactValueInput = bigint | number | boolean | string;

/** One relation in the exposed schema. */
export interface RelationSchema {
  /** The relation name. */
  readonly name: string;
  /** The column types, in order; the arity is `columns.length`. */
  readonly columns: ColumnType[];
  /**
   * A human-readable description of the relation's meaning, or `null` if the
   * backend has none (a grounding gap, not an error).
   */
  readonly doc: string | null;
}

/** The relations a fact base exposes, for grounding a query proposer. */
export interface Schema {
  /** Every relation in the schema. */
  readonly relations: RelationSchema[];
}

/** One supporting fact that participated in a justification. */
export interface SupportTuple {
  /** The relation the supporting fact belongs to. */
  readonly relation: string;
  /** The supporting fact itself. */
  readonly tuple: FactValue[];
}

/** One way a row was produced: the body facts that joined to yield it. */
export interface Justification {
  /** The body facts that joined, in body order. */
  readonly support: SupportTuple[];
}

/** The provenance of a single result row: the row plus every justification. */
export interface RowProvenance {
  /** The result row this provenance explains. */
  readonly row: FactValue[];
  /** Every justification for the row (at least one). */
  readonly justifications: Justification[];
}

/** The result of {@link FactEngine.query}. */
export interface QueryResult {
  /** The distinct result rows. */
  readonly rows: FactValue[][];
  /**
   * Whether evaluation stopped at the cardinality cap. When `true` the rows are
   * a prefix of the full result — a first-class outcome, not an error. A
   * decision keyed on "the result is empty" must distinguish this case.
   */
  readonly truncated: boolean;
  /** One {@link RowProvenance} per row, aligned by index with {@link rows}. */
  readonly provenance: RowProvenance[];
}

/**
 * The stage of the contract at which a query was rejected — the JS face of
 * `fact-query`'s five guarantees.
 *
 * - `parse` — the text is not a valid query.
 * - `schema` — a referenced relation / arity / column type does not exist.
 * - `unsafe` — the query is not range-restricted, or uses a disallowed feature
 *   (e.g. negation).
 * - `eval` — evaluation faulted. **Indeterminate**: treat fail-closed, never as
 *   an empty result.
 * - `engine` — building the engine, ingesting a fact, or running to a fixed
 *   point failed.
 */
export type Stage = "parse" | "schema" | "unsafe" | "eval" | "engine";

/** An error thrown by the substrate, tagged with the {@link Stage} that failed. */
export class FactQueryError extends Error {
  /** Which contract stage rejected the operation. */
  readonly stage: Stage;

  constructor(message: string, stage: Stage) {
    super(message);
    this.name = "FactQueryError";
    this.stage = stage;
  }
}

/** The default cardinality cap when {@link FactEngine.query} is called without one. */
export const DEFAULT_MAX_CARDINALITY = 10_000;

// --- The wire representation crossing the wasm boundary ----------------------

type WireValue = { Int: string } | { Bool: boolean } | { Sym: string };

interface WireSupport {
  relation: string;
  tuple: WireValue[];
}
interface WireProvenance {
  row: WireValue[];
  justifications: { support: WireSupport[] }[];
}
interface WireResult {
  rows: WireValue[][];
  truncated: boolean;
  provenance: WireProvenance[];
}

function toWire(value: FactValueInput): WireValue {
  switch (typeof value) {
    case "bigint":
      return { Int: value.toString() };
    case "number":
      if (!Number.isInteger(value)) {
        throw new TypeError(`fact integer must be an integer, got ${value}`);
      }
      return { Int: value.toString() };
    case "boolean":
      return { Bool: value };
    case "string":
      return { Sym: value };
    default:
      throw new TypeError(`unsupported fact value of type ${typeof value}`);
  }
}

function fromWire(wire: WireValue): FactValue {
  if ("Int" in wire) {
    return BigInt(wire.Int);
  }
  if ("Bool" in wire) {
    return wire.Bool;
  }
  return wire.Sym;
}

function liftResult(raw: WireResult): QueryResult {
  return {
    rows: raw.rows.map((row) => row.map(fromWire)),
    truncated: raw.truncated,
    provenance: raw.provenance.map((rp) => ({
      row: rp.row.map(fromWire),
      justifications: rp.justifications.map((j) => ({
        support: j.support.map((st) => ({
          relation: st.relation,
          tuple: st.tuple.map(fromWire),
        })),
      })),
    })),
  };
}

/**
 * Runs a wasm call, re-wrapping any `stage`-tagged error it throws into a typed
 * {@link FactQueryError}. Non-tagged errors (e.g. input `TypeError`s) pass
 * through unchanged.
 */
function wrap<T>(call: () => T): T {
  try {
    return call();
  } catch (error) {
    const stage = (error as { stage?: unknown }).stage;
    if (error instanceof Error && typeof stage === "string") {
      throw new FactQueryError(error.message, stage as Stage);
    }
    throw error;
  }
}

/**
 * A fact base that evaluates the `fact-query` queries grain in WebAssembly.
 *
 * Build one with {@link FactEngine.fromSource}, populate it with
 * {@link FactEngine.addFact} / {@link FactEngine.addFacts} and
 * {@link FactEngine.run}, then {@link FactEngine.query} it. The schema is
 * available via {@link FactEngine.schema} for grounding a proposer.
 *
 * The instance owns wasm-side memory; call {@link FactEngine.free} when done if
 * you are creating many engines.
 */
export class FactEngine {
  readonly #inner: WasmFactEngine;

  private constructor(inner: WasmFactEngine) {
    this.#inner = inner;
  }

  /**
   * Parses an Ascent program and builds an engine.
   *
   * @throws {@link FactQueryError} (stage `engine`) if the source fails to parse
   * or validate.
   */
  static fromSource(src: string): FactEngine {
    return new FactEngine(wrap(() => WasmFactEngine.fromSource(src)));
  }

  /**
   * Asserts one ground fact into `relation`.
   *
   * @throws `TypeError` if a value is not a valid {@link FactValueInput}, or
   * {@link FactQueryError} (stage `engine`) if the relation is unknown or the
   * arity is wrong.
   */
  addFact(relation: string, values: FactValueInput[]): void {
    const wire = values.map(toWire);
    wrap(() => this.#inner.addFact(relation, wire));
  }

  /**
   * Asserts a batch of facts. Fails on the first bad entry.
   *
   * @throws `TypeError` or {@link FactQueryError} (stage `engine`), as
   * {@link FactEngine.addFact}.
   */
  addFacts(facts: ReadonlyArray<{ relation: string; values: FactValueInput[] }>): void {
    const wire = facts.map((fact) => ({
      relation: fact.relation,
      values: fact.values.map(toWire),
    }));
    wrap(() => this.#inner.addFacts(wire));
  }

  /**
   * Runs the program to a fixed point. Call before {@link FactEngine.query} so
   * the query observes the materialized state.
   *
   * @throws {@link FactQueryError} (stage `engine`) on a stratification or
   * evaluation failure — indeterminate, to be treated fail-closed.
   */
  run(): void {
    wrap(() => this.#inner.run());
  }

  /** The relations the fact base exposes, for grounding a query proposer. */
  schema(): Schema {
    return wrap(() => this.#inner.schema()) as Schema;
  }

  /**
   * Form-checks `text` without evaluating it: parse, then schema-validity and
   * safety / range-restriction. Lets a caller validate a proposed query cheaply
   * before deciding to run it.
   *
   * @throws {@link FactQueryError} with stage `parse`, `schema`, or `unsafe`.
   */
  check(text: string): void {
    wrap(() => this.#inner.check(text));
  }

  /**
   * Parses, form-checks, and evaluates `text` read-only against the current
   * fixed point, capped at `maxCardinality` solutions.
   *
   * @throws {@link FactQueryError} with stage `parse`, `schema`, `unsafe`, or
   * `eval`. An `eval` fault is indeterminate — treat the thrown error
   * fail-closed, never as an empty result.
   */
  query(text: string, maxCardinality: number = DEFAULT_MAX_CARDINALITY): QueryResult {
    const raw = wrap(() => this.#inner.query(text, maxCardinality)) as WireResult;
    return liftResult(raw);
  }

  /** Releases the wasm-side memory backing this engine. */
  free(): void {
    this.#inner.free();
  }
}
