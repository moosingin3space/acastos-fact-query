//! The ad-hoc conjunctive-query primitive (the queries grain): parse a
//! head-free `(outputs) <-- body`, form-check it against the schema, and
//! evaluate it read-only against the current fixed point with provenance and a
//! cardinality cap.

use ascent_jit::{Engine, Value};

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

/// A two-way join evaluates against the materialized state and reports the
/// joined output tuple together with the body tuples that produced it.
#[test]
fn join_yields_tuple_and_provenance() {
    let mut engine = graph(&[(1, 2), (2, 3)]);
    let (outputs, body) = engine
        .parse_query_parts("(x, z) <-- edge(x, y), edge(y, z)")
        .unwrap();
    engine.check_query(&body).unwrap();
    let out = engine.evaluate_query(&outputs, &body, 100).unwrap();

    assert!(!out.truncated);
    assert_eq!(out.solutions.len(), 1, "exactly one 2-hop path: {out:?}");
    let sol = &out.solutions[0];
    assert_eq!(sol.tuple, vec![int(1), int(3)]);
    assert!(
        sol.support
            .contains(&("edge".to_string(), vec![int(1), int(2)]))
    );
    assert!(
        sol.support
            .contains(&("edge".to_string(), vec![int(2), int(3)]))
    );
}

/// An `if` filter restricts the result and is range-checked at the schema level
/// without complaint (the variable is bound by a positive literal).
#[test]
fn filter_restricts_results() {
    let mut engine = graph(&[(1, 2), (1, 3), (2, 3)]);
    let (outputs, body) = engine
        .parse_query_parts("(x, y) <-- edge(x, y), if x == 1")
        .unwrap();
    let out = engine.evaluate_query(&outputs, &body, 100).unwrap();
    let mut rows: Vec<Vec<Value>> = out.solutions.into_iter().map(|s| s.tuple).collect();
    rows.sort();
    assert_eq!(rows, vec![vec![int(1), int(2)], vec![int(1), int(3)]]);
}

/// The cap is on cardinality: collection stops at `max` solutions and the
/// truncation is reported as a first-class outcome, not an error.
#[test]
fn cardinality_cap_truncates() {
    let mut engine = graph(&[(1, 2), (1, 3), (2, 3)]);
    let (outputs, body) = engine.parse_query_parts("(x) <-- edge(x, _)").unwrap();

    let full = engine.evaluate_query(&outputs, &body, 100).unwrap();
    assert_eq!(full.solutions.len(), 3, "three edges, three solutions");
    assert!(!full.truncated);

    let capped = engine.evaluate_query(&outputs, &body, 1).unwrap();
    assert_eq!(capped.solutions.len(), 1);
    assert!(capped.truncated, "hitting the cap is surfaced");
}

/// `check_query` rejects a body that names a relation the schema does not have.
#[test]
fn check_rejects_unknown_relation() {
    let mut engine = graph(&[(1, 2)]);
    let (_outputs, body) = engine.parse_query_parts("(x) <-- node(x)").unwrap();
    assert!(engine.check_query(&body).is_err());
}

/// The query grammar requires a non-empty output tuple and rejects trailing
/// tokens after the body.
#[test]
fn parser_rejects_malformed_queries() {
    let mut engine = graph(&[(1, 2)]);
    assert!(engine.parse_query_parts("() <-- edge(x, y)").is_err());
    assert!(
        engine
            .parse_query_parts("(x) <-- edge(x, y) garbage")
            .is_err()
    );
}
