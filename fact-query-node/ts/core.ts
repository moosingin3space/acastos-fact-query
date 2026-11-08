/**
 * The runtime-independent wrapper shared by the Node and Cloudflare Workers
 * entry points: value conversion, typed errors, and the {@link FactEngine}
 * class over an injected wasm-bindgen binding.
 *
 * @module
 */

// --- The wasm binding --------------------------------------------------------
//
// The wrapper below is runtime-independent. Each entry point (`index.ts` for
// Node, `workers.ts` for Cloudflare Workers) loads its own wasm-bindgen glue
// and hands the generated class to `setBinding` before anything else runs.

/** A cap as it crosses the boundary (a `u32`; larger values saturate). */
function cap(n: number): number {
  if (!Number.isInteger(n) || n < 0) {
    throw new TypeError(`query cap must be a non-negative integer, got ${n}`);
  }
  return Math.min(n, 0xffff_ffff);
}

/** The instance surface of the wasm-bindgen-generated engine. */
export interface InnerEngine {
  addFact(relation: string, values: unknown): void;
  addFacts(facts: unknown): void;
  run(): void;
  schema(): unknown;
  check(text: string): void;
  query(text: string, maxRows: number, maxJustificationsPerRow: number): unknown;
  free(): void;
}

/** The static surface of the wasm-bindgen-generated engine class. */
export interface InnerEngineClass {
  fromSource(src: string): InnerEngine;
}

let binding: InnerEngineClass | undefined;

/**
 * Installs the wasm-bindgen engine class for this runtime. Called once by the
 * package entry point; not part of the public API (the `exports` map does not
 * expose this module).
 */
export function setBinding(inner: InnerEngineClass): void {
  binding = inner;
}

function bound(): InnerEngineClass {
  if (binding === undefined) {
    throw new Error("@acastos/fact-query: wasm binding not initialised; import the package entry point");
  }
  return binding;
}

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

/** One distinct supporting fact: a body fact that joined to produce a row. */
export interface SupportFact {
  /** The relation the fact belongs to. */
  readonly relation: string;
  /** The fact itself. */
  readonly tuple: FactValue[];
}

/**
 * Why each result row is there, normalized: every distinct supporting fact is
 * stored once in {@link Provenance.facts}, and justifications refer to those
 * same objects (compare with `===` to see that two rows share evidence).
 *
 * Every justification has exactly one supporting fact per positive body atom,
 * in body order, so position `i` of a justification always comes from relation
 * {@link Provenance.atoms}`[i]`.
 */
export interface Provenance {
  /** The relation of each support column, one per positive body atom. */
  readonly atoms: readonly string[];
  /** Every distinct supporting fact, each once. */
  readonly facts: readonly SupportFact[];
  /**
   * Whether some row may have justifications that were not returned (the
   * per-row cap was reached, or the row cap stopped evaluation). `false` means
   * every row's evidence is complete.
   */
  readonly truncated: boolean;
  /**
   * Row `row`'s justifications (aligned with {@link QueryResult.rows}): each an
   * array of supporting facts, one per atom. Empty for an out-of-range row.
   */
  justifications(row: number): SupportFact[][];
  /**
   * The raw normalized form: row `r`'s justifications are
   * `offsets[r]..offsets[r + 1]`, and justification `j`'s facts are the indices
   * `support[j * k..(j + 1) * k]` into {@link Provenance.facts}, with
   * `k = atoms.length`.
   */
  readonly offsets: Uint32Array;
  /** See {@link Provenance.offsets}. */
  readonly support: Uint32Array;
}

/** The result of {@link FactEngine.query}. */
export interface QueryResult {
  /** The distinct result rows. */
  readonly rows: FactValue[][];
  /**
   * Whether more rows may exist than were returned (the row cap was reached).
   * When `true` the rows are a prefix of the full result — a first-class
   * outcome, not an error. A decision keyed on "the result is empty" must
   * distinguish this case.
   */
  readonly truncated: boolean;
  /** The rows' provenance, aligned by index with {@link QueryResult.rows}. */
  readonly provenance: Provenance;
}

/**
 * The two independent caps on {@link FactEngine.query}: at most `maxRows`
 * rows, each keeping at most `maxJustificationsPerRow` justifications.
 */
export interface QueryLimits {
  /** Row cap (default {@link DEFAULT_MAX_ROWS}). */
  readonly maxRows?: number;
  /** Per-row justification cap (default {@link DEFAULT_MAX_JUSTIFICATIONS_PER_ROW}). */
  readonly maxJustificationsPerRow?: number;
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

/** The default row cap for {@link FactEngine.query}. */
export const DEFAULT_MAX_ROWS = 10_000;

/** The default per-row justification cap for {@link FactEngine.query}. */
export const DEFAULT_MAX_JUSTIFICATIONS_PER_ROW = 16;

/** @deprecated Use {@link DEFAULT_MAX_ROWS}. */
export const DEFAULT_MAX_CARDINALITY = DEFAULT_MAX_ROWS;

// --- The wire representation crossing the wasm boundary ----------------------

type WireValue = { Int: string } | { Bool: boolean } | { Sym: string };

interface WireProvenance {
  relations: string[];
  atoms: number[];
  facts: { relation: number; tuple: WireValue[] }[];
  truncated: boolean;
  offsets: Uint32Array;
  support: Uint32Array;
}
interface WireResult {
  rows: WireValue[][];
  truncated: boolean;
  provenance: WireProvenance;
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

class ProvenanceView implements Provenance {
  readonly atoms: readonly string[];
  readonly facts: readonly SupportFact[];
  readonly truncated: boolean;
  readonly offsets: Uint32Array;
  readonly support: Uint32Array;

  constructor(raw: WireProvenance) {
    this.atoms = raw.atoms.map((a) => raw.relations[a]);
    // Each distinct fact is converted once; justifications share these objects.
    this.facts = raw.facts.map((f) => ({
      relation: raw.relations[f.relation],
      tuple: f.tuple.map(fromWire),
    }));
    this.truncated = raw.truncated;
    this.offsets = raw.offsets;
    this.support = raw.support;
  }

  justifications(row: number): SupportFact[][] {
    if (!Number.isInteger(row) || row < 0 || row + 1 >= this.offsets.length) {
      return [];
    }
    const k = this.atoms.length;
    const out: SupportFact[][] = [];
    for (let j = this.offsets[row]; j < this.offsets[row + 1]; j++) {
      const facts: SupportFact[] = new Array(k);
      for (let i = 0; i < k; i++) {
        facts[i] = this.facts[this.support[j * k + i]];
      }
      out.push(facts);
    }
    return out;
  }
}

function liftResult(raw: WireResult): QueryResult {
  return {
    rows: raw.rows.map((row) => row.map(fromWire)),
    truncated: raw.truncated,
    provenance: new ProvenanceView(raw.provenance),
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
  readonly #inner: InnerEngine;

  private constructor(inner: InnerEngine) {
    this.#inner = inner;
  }

  /**
   * Parses an Ascent program and builds an engine.
   *
   * @param src the Ascent program (relation declarations and rules).
   *
   * @throws {@link FactQueryError} (stage `engine`) if the source fails to parse
   * or validate.
   */
  static fromSource(src: string): FactEngine {
    return new FactEngine(wrap(() => bound().fromSource(src)));
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
   * fixed point, under two independent caps: at most `maxRows` rows, each
   * keeping at most `maxJustificationsPerRow` justifications. Pass a number as
   * shorthand for `{ maxRows }`.
   *
   * @throws {@link FactQueryError} with stage `parse`, `schema`, `unsafe`, or
   * `eval`. An `eval` fault is indeterminate — treat the thrown error
   * fail-closed, never as an empty result.
   */
  query(text: string, limits: number | QueryLimits = {}): QueryResult {
    const { maxRows = DEFAULT_MAX_ROWS, maxJustificationsPerRow = DEFAULT_MAX_JUSTIFICATIONS_PER_ROW } =
      typeof limits === "number" ? { maxRows: limits } : limits;
    const raw = wrap(() =>
      this.#inner.query(text, cap(maxRows), cap(maxJustificationsPerRow)),
    ) as WireResult;
    return liftResult(raw);
  }

  /** Releases the wasm-side memory backing this engine. */
  free(): void {
    this.#inner.free();
  }
}
