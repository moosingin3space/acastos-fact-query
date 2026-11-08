//! The ad-hoc conjunctive-query primitive (the queries grain): parse a
//! head-free `(outputs) <-- body`, form-check it against the schema, and
//! evaluate it read-only against the current fixed point with provenance under
//! independent row and evidence caps.

use ascent_jit::{Engine, QueryLimits, QueryOutput, Value};

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

/// Generous limits that no test here reaches.
const ALL: QueryLimits = QueryLimits {
    max_rows: 100,
    max_justifications_per_row: 100,
};

/// Resolves every support fact of row `row`'s justifications to values.
fn support_of(engine: &Engine, out: &QueryOutput, row: usize) -> Vec<Vec<(String, Vec<Value>)>> {
    out.justifications(row)
        .map(|support| {
            support
                .iter()
                .map(|&f| {
                    let (rel, tuple) = engine.fact(out.facts()[f as usize]).unwrap();
                    (rel.to_owned(), tuple.to_vec())
                })
                .collect()
        })
        .collect()
}

/// A two-way join evaluates against the materialized state and reports the
/// joined output row together with the body facts that produced it, one per
/// positive atom in body order.
#[test]
fn join_yields_row_and_provenance() {
    let mut engine = graph(&[(1, 2), (2, 3)]);
    let (outputs, body) = engine
        .parse_query_parts("(x, z) <-- edge(x, y), edge(y, z)")
        .unwrap();
    engine.check_query(&body).unwrap();
    let out = engine.evaluate_query(&outputs, &body, ALL).unwrap();

    assert!(!out.rows_truncated() && !out.evidence_truncated());
    assert_eq!(out.len(), 1, "exactly one 2-hop path: {out:?}");
    assert_eq!(out.row(0), Some(&[int(1), int(3)][..]));
    assert_eq!(out.atoms(), ["edge", "edge"]);
    assert_eq!(
        support_of(&engine, &out, 0),
        vec![vec![
            ("edge".to_owned(), vec![int(1), int(2)]),
            ("edge".to_owned(), vec![int(2), int(3)]),
        ]]
    );
}

/// An `if` filter restricts the result.
#[test]
fn filter_restricts_results() {
    let mut engine = graph(&[(1, 2), (1, 3), (2, 3)]);
    let (outputs, body) = engine
        .parse_query_parts("(x, y) <-- edge(x, y), if x == 1")
        .unwrap();
    let out = engine.evaluate_query(&outputs, &body, ALL).unwrap();
    let mut rows: Vec<Vec<Value>> = out.rows().map(<[Value]>::to_vec).collect();
    rows.sort();
    assert_eq!(rows, vec![vec![int(1), int(2)], vec![int(1), int(3)]]);
}

/// Join combinations producing the same row are grouped into one row with one
/// justification each, and a fact supporting several justifications is stored
/// once.
#[test]
fn rows_are_grouped_and_facts_shared() {
    let mut engine = graph(&[(1, 2), (1, 3), (2, 3)]);
    let (outputs, body) = engine
        .parse_query_parts("(x) <-- edge(x, y), edge(y, z)")
        .unwrap();
    let out = engine.evaluate_query(&outputs, &body, ALL).unwrap();
    // 1→2→3 is the only 2-hop path, so one row with one justification.
    assert_eq!(out.len(), 1);
    assert_eq!(out.justifications(0).len(), 1);

    let (outputs, body) = engine.parse_query_parts("(x) <-- edge(x, _)").unwrap();
    let out = engine.evaluate_query(&outputs, &body, ALL).unwrap();
    let mut rows: Vec<Vec<Value>> = out.rows().map(<[Value]>::to_vec).collect();
    rows.sort();
    assert_eq!(rows, vec![vec![int(1)], vec![int(2)]]);
    let one = out.rows().position(|r| r == [int(1)]).unwrap();
    assert_eq!(out.justifications(one).len(), 2, "edges (1,2) and (1,3)");
    assert_eq!(out.facts().len(), 3, "each edge stored once");
}

/// The per-row cap bounds evidence without dropping rows: the rows are
/// complete, and only the evidence is flagged as truncated.
#[test]
fn justification_cap_truncates_evidence_only() {
    let mut engine = graph(&[(1, 2), (1, 3), (1, 4), (2, 3)]);
    let (outputs, body) = engine.parse_query_parts("(x) <-- edge(x, _)").unwrap();
    let limits = QueryLimits {
        max_rows: 100,
        max_justifications_per_row: 1,
    };
    let out = engine.evaluate_query(&outputs, &body, limits).unwrap();
    assert_eq!(out.len(), 2, "both rows are present");
    assert!(!out.rows_truncated());
    assert!(out.evidence_truncated(), "row (1) had three justifications");
    assert!(
        out.rows()
            .enumerate()
            .all(|(i, _)| out.justifications(i).len() == 1)
    );
}

/// The row cap stops evaluation and flags both rows and evidence as possibly
/// incomplete — a first-class outcome, not an error.
#[test]
fn row_cap_truncates_rows_and_evidence() {
    let mut engine = graph(&[(1, 2), (2, 3), (3, 4)]);
    let (outputs, body) = engine.parse_query_parts("(x) <-- edge(x, _)").unwrap();

    let full = engine.evaluate_query(&outputs, &body, ALL).unwrap();
    assert_eq!(full.len(), 3);
    assert!(!full.rows_truncated() && !full.evidence_truncated());

    let limits = QueryLimits {
        max_rows: 1,
        max_justifications_per_row: 100,
    };
    let capped = engine.evaluate_query(&outputs, &body, limits).unwrap();
    assert_eq!(capped.len(), 1);
    assert!(capped.rows_truncated(), "hitting the row cap is surfaced");
    assert!(
        capped.evidence_truncated(),
        "evidence was not fully enumerated"
    );
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
