//! Column-type validation at the ingestion boundary.
//!
//! `add_fact` and `speculate` candidates are type-checked against each relation's
//! declared column types; the engine's own derivations are not (a well-typed program
//! keeps them correct by construction), so a derived tuple over a typed relation is
//! exercised here too.

use ascent_jit::eval::EvalError;
use ascent_jit::{Engine, Error, Value};

fn engine() -> Engine {
    Engine::from_source_interpreted(
        "relation node(sym, int);
         relation reachable(sym, int);
         reachable(x, n) <-- node(x, n);",
    )
    .expect("load")
}

#[test]
fn add_fact_accepts_a_well_typed_tuple() {
    let mut e = engine();
    let s = e.intern("a");
    e.add_fact("node", vec![Value::Sym(s), Value::Int(1)])
        .expect("well-typed fact is accepted");
    e.run().expect("run");
    // The derived tuple flowed through the unchecked derive path without trouble.
    assert_eq!(
        e.query("reachable"),
        vec![vec![Value::Sym(s), Value::Int(1)]]
    );
}

#[test]
fn add_fact_rejects_a_wrong_column_type() {
    let mut e = engine();
    // Column 0 is `sym`, but an `int` is supplied.
    let err = e
        .add_fact("node", vec![Value::Int(1), Value::Int(2)])
        .expect_err("type mismatch is rejected");
    match err {
        Error::Eval(EvalError::ColumnType {
            relation, column, ..
        }) => {
            assert_eq!(relation, "node");
            assert_eq!(column, 0);
        }
        other => panic!("expected ColumnType, got {other:?}"),
    }
}

#[test]
fn add_fact_still_enforces_arity() {
    let mut e = engine();
    let s = e.intern("a");
    let err = e
        .add_fact("node", vec![Value::Sym(s)])
        .expect_err("arity mismatch is rejected");
    assert!(matches!(err, Error::Eval(EvalError::Arity { .. })));
}

#[test]
fn speculate_rejects_a_wrong_typed_candidate() {
    let mut e = engine();
    let err = e
        .speculate(&[("node", vec![Value::Int(1), Value::Int(2)])])
        .expect_err("type mismatch is rejected before evaluation");
    assert!(matches!(err, Error::Eval(EvalError::ColumnType { .. })));
}
