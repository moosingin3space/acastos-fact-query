//! Speculative ("fork / run / discard") evaluation: the substrate for "what-if"
//! checks and previewing the consequences of a proposed change.

use ascent_jit::{Engine, Value};

fn int(n: i64) -> Value {
    Value::Int(n)
}

const TRANSITIVE_CLOSURE: &str = "relation edge(int, int);
     relation path(int, int);
     path(x, y) <-- edge(x, y);
     path(x, z) <-- edge(x, y), path(y, z);";

/// Speculation reports the tuples a candidate fact would make derivable, and
/// leaves the engine untouched.
#[test]
fn speculation_reports_delta_without_mutating() {
    let mut engine = Engine::from_source(TRANSITIVE_CLOSURE).unwrap();
    engine.add_fact("edge", vec![int(1), int(2)]).unwrap();
    engine.add_fact("edge", vec![int(2), int(3)]).unwrap();
    engine.run().unwrap();

    let paths_before = engine.query("path").len();

    // What if we also had edge(3, 4)?
    let consequences = engine.speculate(&[("edge", vec![int(3), int(4)])]).unwrap();
    let new_paths = consequences.added("path");

    // 3->4 unlocks path(3,4), path(2,4), path(1,4).
    assert!(new_paths.contains(&vec![int(3), int(4)]));
    assert!(new_paths.contains(&vec![int(2), int(4)]));
    assert!(new_paths.contains(&vec![int(1), int(4)]));
    // The candidate edge itself is a new derivation in `edge`.
    assert_eq!(consequences.added("edge"), &[vec![int(3), int(4)]]);

    // The engine is unchanged: the fork was discarded.
    assert_eq!(engine.query("path").len(), paths_before);
    assert!(!engine.query("path").contains(&vec![int(1), int(4)]));
}

const VIOLATION_SHAPE: &str = "relation candidate(int);
     relation forbidden(int);
     relation violation(int);
     violation(x) <-- candidate(x), forbidden(x);";

/// The constraint-check shape: does adding a candidate fact make a `violation`
/// relation gain a tuple?
#[test]
fn speculation_detects_a_would_be_violation() {
    let mut engine = Engine::from_source(VIOLATION_SHAPE).unwrap();
    engine.add_fact("forbidden", vec![int(7)]).unwrap();
    engine.run().unwrap();
    assert!(engine.query("violation").is_empty());

    // A candidate that trips the constraint.
    let bad = engine.speculate(&[("candidate", vec![int(7)])]).unwrap();
    assert_eq!(bad.added("violation"), &[vec![int(7)]]);

    // A candidate that does not.
    let ok = engine.speculate(&[("candidate", vec![int(5)])]).unwrap();
    assert!(ok.added("violation").is_empty());
}

/// A candidate that derives nothing new yields empty consequences.
#[test]
fn speculation_with_no_effect_is_empty() {
    let mut engine = Engine::from_source(TRANSITIVE_CLOSURE).unwrap();
    engine.add_fact("edge", vec![int(1), int(2)]).unwrap();
    engine.run().unwrap();

    // edge(1,2) is already present and derives nothing new.
    let consequences = engine.speculate(&[("edge", vec![int(1), int(2)])]).unwrap();
    assert!(consequences.is_empty(), "no new derivations expected");
}

/// An unknown relation in a candidate is an error, not a silent no-op.
#[test]
fn speculation_rejects_unknown_relation() {
    let mut engine = Engine::from_source(TRANSITIVE_CLOSURE).unwrap();
    assert!(engine.speculate(&[("nope", vec![int(1)])]).is_err());
}
