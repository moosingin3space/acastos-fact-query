//! The queries-grain contract, exercised through the canonical
//! `impl FactStore for Engine`: parse → check (schema-valid + range-restricted)
//! → bounded read-only eval with provenance. The crate guarantees *form*, never
//! intent, so these tests assert on shape, safety, and bounds — not on meaning.

use ascent_jit::{Engine, Type, Value};
use fact_query::{Cardinality, FactStore};

fn int(n: i64) -> Value {
    Value::Int(n)
}

const EDGES: &str = "relation edge(int, int);";

fn graph(edges: &[(i64, i64)]) -> Engine {
    let mut engine = Engine::from_source(EDGES).unwrap();
    for (a, b) in edges {
        engine.add_fact("edge", vec![int(*a), int(*b)]).unwrap();
    }
    engine.run().unwrap();
    engine
}

/// `schema()` exposes relations with their arity and column types. Doc-strings
/// are `None` (the engine IR has none yet — a known grounding gap).
#[test]
fn schema_describes_relations() {
    let engine = graph(&[]);
    let schema = engine.schema();
    let edge = schema.relation("edge").expect("edge is in the schema");
    assert_eq!(edge.columns, vec![Type::Int, Type::Int]);
    assert_eq!(edge.arity(), 2);
    assert!(edge.doc.is_none());
}

/// A well-formed, safe join parses, checks, and evaluates to distinct rows, each
/// carrying the provenance for why it is there.
#[test]
fn parse_check_eval_with_provenance() {
    let mut engine = graph(&[(1, 2), (1, 3), (2, 3)]);
    let query = engine.parse_query("(x) <-- edge(x, _)").unwrap();
    engine.check(&query).unwrap();
    let (results, provenance) = engine.eval(&query, Cardinality::new(100)).unwrap();

    let mut rows = results.rows().to_vec();
    rows.sort();
    assert_eq!(rows, vec![vec![int(1)], vec![int(2)]], "distinct sources");
    assert!(!results.is_truncated());

    // Provenance is aligned with the rows and node 1 has two justifications
    // (edge(1,2) and edge(1,3)).
    assert_eq!(provenance.rows().len(), results.len());
    let node_one = provenance
        .rows()
        .iter()
        .find(|p| p.row == vec![int(1)])
        .expect("provenance for row (1)");
    assert_eq!(node_one.justifications.len(), 2);
    assert!(node_one.justifications.iter().all(|j| {
        j.support
            .iter()
            .any(|s| s.relation == "edge" && s.tuple[0] == int(1))
    }));
}

/// Guarantee 3: an output variable bound by no positive body literal is rejected
/// by `check` as unsafe — caught before evaluation.
#[test]
fn check_rejects_unrestricted_output() {
    let mut engine = graph(&[(1, 2)]);
    let query = engine.parse_query("(z) <-- edge(x, y)").unwrap();
    let err = engine.check(&query).unwrap_err();
    assert!(
        matches!(err, fact_query::FactQueryError::Unsafe(_)),
        "expected an unsafe-query error, got {err:?}"
    );
}

/// A filter variable bound by no positive literal is likewise unsafe.
#[test]
fn check_rejects_unrestricted_filter_var() {
    let mut engine = graph(&[(1, 2)]);
    let query = engine.parse_query("(x) <-- edge(x, y), if z == 1").unwrap();
    assert!(matches!(
        engine.check(&query).unwrap_err(),
        fact_query::FactQueryError::Unsafe(_)
    ));
}

/// Negation is outside the conjunctive fragment and is rejected by `check`.
#[test]
fn check_rejects_negation() {
    let mut engine = graph(&[(1, 2)]);
    let query = engine
        .parse_query("(x, y) <-- edge(x, y), !edge(y, x)")
        .unwrap();
    assert!(matches!(
        engine.check(&query).unwrap_err(),
        fact_query::FactQueryError::Unsafe(_)
    ));
}

/// Guarantee 5: evaluation is cardinality-bounded, and hitting the cap is a
/// first-class outcome on the result set, not an error.
#[test]
fn eval_is_cardinality_bounded() {
    let mut engine = graph(&[(1, 2), (1, 3), (2, 3)]);
    let query = engine.parse_query("(x, y) <-- edge(x, y)").unwrap();
    let (results, _prov) = engine.eval(&query, Cardinality::new(2)).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.is_truncated());
}

/// A relation the schema does not have is a schema error from `check`.
#[test]
fn check_rejects_unknown_relation() {
    let mut engine = graph(&[(1, 2)]);
    let query = engine.parse_query("(x) <-- node(x)").unwrap();
    assert!(matches!(
        engine.check(&query).unwrap_err(),
        fact_query::FactQueryError::Schema(_)
    ));
}
