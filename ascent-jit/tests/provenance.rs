//! Provenance: `explain` recovers the one-step justification (rule + support)
//! for a derived tuple against the materialized database, and support facts
//! are handles that can be explained in turn.

use ascent_jit::{Derivation, Engine, Value};

fn int(n: i64) -> Value {
    Value::Int(n)
}

const TRANSITIVE_CLOSURE: &str = "relation edge(int, int);
     relation path(int, int);
     path(x, y) <-- edge(x, y);
     path(x, z) <-- edge(x, y), path(y, z);";

/// A derivation's support, resolved to `(relation, tuple)` values.
fn resolved(engine: &Engine, d: &Derivation) -> Vec<(String, Vec<Value>)> {
    d.support
        .iter()
        .map(|&f| {
            let (rel, tuple) = engine.fact(f).unwrap();
            (rel.to_owned(), tuple.to_vec())
        })
        .collect()
}

fn closure_engine() -> Engine {
    let mut engine = Engine::from_source(TRANSITIVE_CLOSURE).unwrap();
    engine.add_fact("edge", vec![int(1), int(2)]).unwrap();
    engine.add_fact("edge", vec![int(2), int(3)]).unwrap();
    engine.run().unwrap();
    engine
}

/// A directly-derived tuple: path(1,2) comes from the base rule (index 0) with
/// edge(1,2) as its sole support.
#[test]
fn explains_a_base_rule_derivation() {
    let engine = closure_engine();
    let derivations = engine.explain("path", &[int(1), int(2)]).unwrap();

    assert!(
        derivations.iter().any(|d| d.rule == 0
            && resolved(&engine, d) == vec![("edge".to_string(), vec![int(1), int(2)])]),
        "path(1,2) should be justified by rule 0 via edge(1,2); got {derivations:?}"
    );
}

/// A transitively-derived tuple: path(1,3) comes from the recursive rule
/// (index 1) supported by edge(1,2) and path(2,3).
#[test]
fn explains_a_recursive_derivation() {
    let engine = closure_engine();
    let derivations = engine.explain("path", &[int(1), int(3)]).unwrap();

    let recursive = derivations
        .iter()
        .find(|d| d.rule == 1)
        .expect("path(1,3) should have a derivation via the recursive rule (1)");
    assert_eq!(
        resolved(&engine, recursive),
        vec![
            ("edge".to_string(), vec![int(1), int(2)]),
            ("path".to_string(), vec![int(2), int(3)]),
        ],
        "support is edge(1,2) then path(2,3), in body order"
    );
}

/// A tuple that is not derivable yields no derivations rather than an error.
#[test]
fn unprovable_tuple_has_no_derivations() {
    let engine = closure_engine();
    assert_eq!(engine.explain("path", &[int(9), int(9)]).unwrap(), []);
}

/// Explaining an unknown relation is an error.
#[test]
fn explain_rejects_unknown_relation() {
    let engine = closure_engine();
    assert!(engine.explain("nope", &[int(1)]).is_err());
}

/// Support facts are handles: explaining one walks the derivation tree a step
/// further, down to base facts, which have no derivations.
#[test]
fn explanations_chain_through_fact_handles() {
    let engine = closure_engine();
    let top = engine.explain("path", &[int(1), int(3)]).unwrap();
    let recursive = top.iter().find(|d| d.rule == 1).unwrap();
    let path_2_3 = recursive.support[1];
    assert_eq!(engine.fact(path_2_3), Some(("path", &[int(2), int(3)][..])));

    let next = engine.explain_fact(path_2_3).unwrap();
    let base = next
        .iter()
        .find(|d| d.rule == 0)
        .expect("path(2,3) via the base rule");
    let edge_2_3 = base.support[0];
    assert_eq!(engine.fact(edge_2_3), Some(("edge", &[int(2), int(3)][..])));
    assert!(
        engine.explain_fact(edge_2_3).unwrap().is_empty(),
        "edges are base facts"
    );
}
