//! Node.js / TypeScript bindings for [`fact-query`](https://docs.rs/fact-query).
//!
//! This crate is the `wasm32` host that lets a Node.js (or any `WebAssembly`)
//! runtime drive the `fact-query` **queries grain** — parse, form-check, and
//! bounded read-only evaluation of conjunctive queries with provenance — over an
//! `ascent-jit` fact base. It carries **no policy**: like `fact-query` itself it
//! never names an application's denial, trust, or origin vocabulary; it only
//! moves typed tuples and queries across the JS boundary. The dependency
//! direction (this crate depends on `fact-query`, never the reverse, and on no
//! application) is the governance-free guarantee.
//!
//! The expression tier runs on the **host's own `WebAssembly` engine** via
//! [`ascent_jit_web`]'s `WebExecutor`, so there is no native `wasmtime`
//! dependency: the whole substrate evaluates in place inside the wasm module.
//!
//! # The boundary
//!
//! Values cross as a small, unambiguous wire form (see [`WireValue`]): an
//! integer is a **decimal string** (the engine's value model is `i64`; a string
//! is exact where a JS `number` is not), a boolean is a boolean, and a symbol is
//! its **resolved string** — never a raw interner id, which is meaningless
//! outside the engine. Symbol columns are interned on the way in and resolved on
//! the way out. The ergonomic `bigint | boolean | string` shaping lives in the
//! TypeScript wrapper layered on top of this module.
//!
//! Errors thrown across the boundary carry a `stage` property
//! (`"parse" | "schema" | "unsafe" | "eval" | "engine"`) so the contract's
//! distinct guarantees survive into JS. An evaluation fault is **thrown**, never
//! reported as an empty result — preserving `fact-query`'s fail-closed contract.

#![cfg(target_arch = "wasm32")]

use ascent_jit::{Engine, Type, Value};
use fact_query::{Cardinality, FactQueryError, FactStore};
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

/// A single column value as it crosses the JS boundary.
///
/// Externally tagged, so JS sees `{ "Int": "42" }`, `{ "Bool": true }`, or
/// `{ "Sym": "name" }`. `Int` is a decimal string because the engine's integers
/// are `i64`, which a JS `number` cannot hold losslessly; the TypeScript wrapper
/// converts it to a `bigint`.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum WireValue {
    /// A signed 64-bit integer, as a decimal string.
    Int(String),
    /// A boolean.
    Bool(bool),
    /// A symbol, as its resolved string.
    Sym(String),
}

/// One fact to assert: the relation it belongs to and its column values.
#[derive(Debug, Deserialize)]
struct FactInput {
    /// The relation the fact is a row of.
    relation: String,
    /// The column values, in declared order.
    values: Vec<WireValue>,
}

/// One relation in the exposed schema (for grounding a query proposer).
#[derive(Debug, Serialize)]
struct RelationSchemaOut {
    /// The relation name.
    name: String,
    /// The column types, in order: `"int"`, `"bool"`, or `"sym"`.
    columns: Vec<&'static str>,
    /// A human-readable description of the relation's meaning, if the backend
    /// has one (`null` is a grounding gap, not an error).
    doc: Option<String>,
}

/// The relations a fact base exposes.
#[derive(Debug, Serialize)]
struct SchemaOut {
    /// Every relation in the schema.
    relations: Vec<RelationSchemaOut>,
}

/// One supporting fact in a justification.
#[derive(Debug, Serialize)]
struct SupportTupleOut {
    /// The relation the supporting fact belongs to.
    relation: String,
    /// The supporting fact itself.
    tuple: Vec<WireValue>,
}

/// One way a row was produced: the body facts that joined to yield it.
#[derive(Debug, Serialize)]
struct JustificationOut {
    /// The body facts that joined, in body order.
    support: Vec<SupportTupleOut>,
}

/// The provenance of one result row: the row plus every justification found.
#[derive(Debug, Serialize)]
struct RowProvenanceOut {
    /// The result row this provenance explains.
    row: Vec<WireValue>,
    /// Every justification for the row (at least one).
    justifications: Vec<JustificationOut>,
}

/// The full result of a query: rows, truncation flag, and aligned provenance.
#[derive(Debug, Serialize)]
struct QueryResultOut {
    /// The distinct result rows.
    rows: Vec<Vec<WireValue>>,
    /// Whether evaluation stopped at the cardinality cap (a prefix of the full
    /// result, a first-class outcome — not an error).
    truncated: bool,
    /// One entry per row, aligned by index with `rows`.
    provenance: Vec<RowProvenanceOut>,
}

/// A fact base that evaluates the `fact-query` queries grain inside wasm.
///
/// Wraps an [`ascent_jit::Engine`] whose expression tier runs on the host
/// `WebAssembly` engine. Build one with [`FactEngine::from_source`], populate it
/// with [`add_fact`](FactEngine::add_fact) / [`add_facts`](FactEngine::add_facts)
/// and [`run`](FactEngine::run), then query it with [`query`](FactEngine::query).
#[wasm_bindgen]
#[derive(Debug)]
pub struct FactEngine {
    engine: Engine,
}

#[wasm_bindgen]
impl FactEngine {
    /// Parses an Ascent program `src` and builds an engine over the host
    /// `WebAssembly` engine.
    ///
    /// # Errors
    ///
    /// Throws (stage `"engine"`) if `src` fails to parse or validate.
    #[wasm_bindgen(js_name = fromSource)]
    pub fn from_source(src: &str) -> Result<FactEngine, JsValue> {
        let engine = ascent_jit_web::engine_from_source(src)
            .map_err(|e| stage_error("engine", &e.to_string()))?;
        Ok(FactEngine { engine })
    }

    /// Asserts one ground fact into `relation`. `values` is an array of
    /// [`WireValue`]; symbol columns are interned into the engine.
    ///
    /// # Errors
    ///
    /// Throws if `values` is malformed, an integer string does not parse, or the
    /// relation is unknown / the arity is wrong (stage `"engine"`).
    #[wasm_bindgen(js_name = addFact)]
    pub fn add_fact(&mut self, relation: &str, values: JsValue) -> Result<(), JsValue> {
        let wires: Vec<WireValue> = serde_wasm_bindgen::from_value(values)
            .map_err(|e| stage_error("engine", &e.to_string()))?;
        let tuple = self.lower_tuple(wires)?;
        self.engine
            .add_fact(relation, tuple)
            .map_err(|e| stage_error("engine", &e.to_string()))
    }

    /// Asserts a batch of facts. `facts` is an array of
    /// `{ relation, values }` objects.
    ///
    /// # Errors
    ///
    /// Throws on the first malformed entry, unparsable integer, or unknown
    /// relation / arity mismatch (stage `"engine"`).
    #[wasm_bindgen(js_name = addFacts)]
    pub fn add_facts(&mut self, facts: JsValue) -> Result<(), JsValue> {
        let batch: Vec<FactInput> = serde_wasm_bindgen::from_value(facts)
            .map_err(|e| stage_error("engine", &e.to_string()))?;
        for fact in batch {
            let tuple = self.lower_tuple(fact.values)?;
            self.engine
                .add_fact(&fact.relation, tuple)
                .map_err(|e| stage_error("engine", &e.to_string()))?;
        }
        Ok(())
    }

    /// Runs the program to a fixed point. Call before querying so the query
    /// observes the materialized state.
    ///
    /// # Errors
    ///
    /// Throws (stage `"engine"`) on a stratification or evaluation failure. Such
    /// a fault is indeterminate — treat it fail-closed, not as "nothing derived".
    pub fn run(&mut self) -> Result<(), JsValue> {
        self.engine
            .run()
            .map_err(|e| stage_error("engine", &e.to_string()))
    }

    /// The relations the fact base exposes — name, column types, and (where
    /// available) doc-strings — the material a proposer needs to ground a query.
    ///
    /// # Errors
    ///
    /// Throws if the schema cannot be serialized across the boundary.
    pub fn schema(&self) -> Result<JsValue, JsValue> {
        let relations = self
            .engine
            .program()
            .relations
            .iter()
            .map(|decl| RelationSchemaOut {
                name: decl.name.clone(),
                columns: decl.schema.iter().copied().map(type_str).collect(),
                // The engine's IR carries no relation doc-strings yet: a known
                // grounding gap, surfaced as `null`, not a contract violation.
                doc: None,
            })
            .collect();
        let schema = SchemaOut { relations };
        to_js(&schema)
    }

    /// Form-checks `text` without evaluating it: parses it (stage `"parse"`),
    /// then checks schema-validity (stage `"schema"`) and safety /
    /// range-restriction (stage `"unsafe"`). Lets a caller validate a proposed
    /// query cheaply before deciding to run it.
    ///
    /// # Errors
    ///
    /// Throws with the corresponding `stage` if any of those guarantees fail.
    pub fn check(&mut self, text: &str) -> Result<(), JsValue> {
        let query = self.engine.parse_query(text).map_err(query_error)?;
        FactStore::check(&self.engine, &query).map_err(query_error)
    }

    /// Parses, form-checks, and evaluates `text` read-only against the current
    /// fixed point, capped at `max_cardinality` solutions. Returns the distinct
    /// rows, whether the result was truncated at the cap, and per-row provenance.
    ///
    /// # Errors
    ///
    /// Throws with a `stage` of `"parse"`, `"schema"`, `"unsafe"`, or `"eval"`
    /// per the guarantee that failed. An `"eval"` fault is indeterminate — a
    /// safety-conscious caller must treat the thrown error fail-closed, never as
    /// an empty result.
    pub fn query(&mut self, text: &str, max_cardinality: u32) -> Result<JsValue, JsValue> {
        let query = self.engine.parse_query(text).map_err(query_error)?;
        FactStore::check(&self.engine, &query).map_err(query_error)?;
        let (results, provenance) = self
            .engine
            .eval(&query, Cardinality::new(max_cardinality as usize))
            .map_err(query_error)?;

        let rows = results
            .rows()
            .iter()
            .map(|row| self.lift_row(row))
            .collect::<Result<Vec<_>, _>>()?;
        let provenance = provenance
            .rows()
            .iter()
            .map(|rp| self.lift_provenance(rp))
            .collect::<Result<Vec<_>, _>>()?;

        let out = QueryResultOut {
            rows,
            truncated: results.is_truncated(),
            provenance,
        };
        to_js(&out)
    }
}

impl FactEngine {
    /// Lowers a tuple of wire values into engine values, interning symbols.
    fn lower_tuple(&mut self, wires: Vec<WireValue>) -> Result<Vec<Value>, JsValue> {
        wires
            .into_iter()
            .map(|w| self.lower_value(w))
            .collect::<Result<Vec<_>, _>>()
    }

    /// Lowers one wire value into an engine value, interning a symbol's string.
    fn lower_value(&mut self, wire: WireValue) -> Result<Value, JsValue> {
        Ok(match wire {
            WireValue::Int(s) => {
                let i = s
                    .parse::<i64>()
                    .map_err(|_| stage_error("engine", &format!("invalid integer `{s}`")))?;
                Value::Int(i)
            }
            WireValue::Bool(b) => Value::Bool(b),
            WireValue::Sym(s) => Value::Sym(self.engine.intern(&s)),
        })
    }

    /// Lifts an engine row into wire values, resolving symbol ids to strings.
    fn lift_row(&self, row: &[Value]) -> Result<Vec<WireValue>, JsValue> {
        row.iter().map(|v| self.lift_value(*v)).collect()
    }

    /// Lifts one engine value into a wire value, resolving a symbol to its
    /// string.
    fn lift_value(&self, value: Value) -> Result<WireValue, JsValue> {
        Ok(match value {
            Value::Int(i) => WireValue::Int(i.to_string()),
            Value::Bool(b) => WireValue::Bool(b),
            Value::Sym(s) => {
                let name = self.engine.resolve(s).ok_or_else(|| {
                    // A result symbol that does not resolve is an engine
                    // invariant break; surface it fail-closed rather than guess.
                    stage_error("eval", "result contained an unresolvable symbol")
                })?;
                WireValue::Sym(name.to_owned())
            }
        })
    }

    /// Lifts the provenance of one row into its serializable form.
    fn lift_provenance(&self, rp: &fact_query::RowProvenance) -> Result<RowProvenanceOut, JsValue> {
        let row = self.lift_row(&rp.row)?;
        let justifications = rp
            .justifications
            .iter()
            .map(|j| {
                let support = j
                    .support
                    .iter()
                    .map(|st| {
                        Ok(SupportTupleOut {
                            relation: st.relation.clone(),
                            tuple: self.lift_row(&st.tuple)?,
                        })
                    })
                    .collect::<Result<Vec<_>, JsValue>>()?;
                Ok(JustificationOut { support })
            })
            .collect::<Result<Vec<_>, JsValue>>()?;
        Ok(RowProvenanceOut {
            row,
            justifications,
        })
    }
}

/// Serializes a value to a `JsValue`, emitting `null` (not `undefined`) for a
/// missing/`None` field so it matches the TypeScript layer's `T | null` types.
fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_missing_as_null(true);
    value
        .serialize(&serializer)
        .map_err(|e| stage_error("engine", &e.to_string()))
}

/// The wire name of a column type.
fn type_str(ty: Type) -> &'static str {
    match ty {
        Type::Int => "int",
        Type::Bool => "bool",
        Type::Sym => "sym",
    }
}

/// Maps a [`FactQueryError`] to a thrown JS error tagged with the contract stage
/// that failed.
fn query_error(error: FactQueryError) -> JsValue {
    let (stage, message) = match error {
        FactQueryError::Parse(m) => ("parse", m),
        FactQueryError::Schema(m) => ("schema", m),
        FactQueryError::Unsafe(m) => ("unsafe", m),
        FactQueryError::Eval(m) => ("eval", m),
    };
    stage_error(stage, &message)
}

/// Builds a JS `Error` carrying a `stage` property, so the caller can branch on
/// which guarantee failed without parsing the message.
fn stage_error(stage: &str, message: &str) -> JsValue {
    let error = js_sys::Error::new(message);
    // Best-effort: a failure to attach `stage` still leaves a usable Error.
    let _ = js_sys::Reflect::set(
        &error,
        &JsValue::from_str("stage"),
        &JsValue::from_str(stage),
    );
    error.into()
}
