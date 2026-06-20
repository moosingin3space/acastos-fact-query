//! Provenance: `explain` recovers the one-step justification (rule + support)
//! for a derived tuple against the materialized database.

use ascent_jit::{Engine, Value};

fn int(n: i64) -> Value {
    Value::Int(n)
}

const TRANSITIVE_CLOSURE: &str = "relation edge(int, int);
     relation path(int, int);
     path(x, y) <-- edge(x, y);
     path(x, z) <-- edge(x, y), path(y, z);";

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
    let mut engine = closure_engine();
    let derivations = engine.explain("path", &[int(1), int(2)]).unwrap();

    assert!(
        derivations
            .iter()
            .any(|d| d.rule == 0 && d.support == vec![("edge".to_string(), vec![int(1), int(2)])]),
        "path(1,2) should be justified by rule 0 via edge(1,2); got {derivations:?}"
    );
}

/// A transitively-derived tuple: path(1,3) comes from the recursive rule
/// (index 1) supported by edge(1,2) and path(2,3).
#[test]
fn explains_a_recursive_derivation() {
    let mut engine = closure_engine();
    let derivations = engine.explain("path", &[int(1), int(3)]).unwrap();

    let recursive = derivations
        .iter()
        .find(|d| d.rule == 1)
        .expect("path(1,3) should have a derivation via the recursive rule (1)");
    assert!(
        recursive
            .support
            .contains(&("edge".to_string(), vec![int(1), int(2)])),
        "support should include edge(1,2); got {:?}",
        recursive.support
    );
    assert!(
        recursive
            .support
            .contains(&("path".to_string(), vec![int(2), int(3)])),
        "support should include path(2,3); got {:?}",
        recursive.support
    );
}

/// A tuple that is not derivable yields no derivations rather than an error.
#[test]
fn unprovable_tuple_has_no_derivations() {
    let mut engine = closure_engine();
    assert!(
        engine
            .explain("path", &[int(9), int(9)])
            .unwrap()
            .is_empty()
    );
}

/// Explaining an unknown relation is an error.
#[test]
fn explain_rejects_unknown_relation() {
    let mut engine = closure_engine();
    assert!(engine.explain("nope", &[int(1)]).is_err());
}
