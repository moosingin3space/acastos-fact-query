//! Recursion limits: pathologically deep input is rejected with an error (fail
//! closed) instead of overflowing the stack, and a rule body of any length is
//! evaluated without overflow (the body walk grows its stack on demand).

use std::collections::HashMap;

use ascent_jit::expr::{Expr, MAX_EXPR_DEPTH, UnOp};
use ascent_jit::{Engine, ExprError, Value};

/// A program whose single rule derives `q(<head>)` from any `r` tuple.
fn program_with_head(head: &str) -> String {
    format!("relation r(int); relation q(int); q({head}) <-- r(_);")
}

/// `1 + 1 + ... + 1` with `n` terms: a left-leaning tree of depth `n`, built by
/// the parser's loop rather than by recursion.
fn flat_sum(n: usize) -> String {
    vec!["1"; n].join(" + ")
}

fn assert_too_deep(src: &str) {
    match Engine::from_source(src) {
        Err(e) => assert!(
            e.to_string().contains("nested deeper than"),
            "unexpected error: {e}"
        ),
        Ok(_) => panic!("a too-deep expression was accepted"),
    }
}

#[test]
fn an_expression_at_the_limit_parses_and_evaluates() {
    let mut engine = Engine::from_source(&program_with_head(&flat_sum(MAX_EXPR_DEPTH))).unwrap();
    engine.add_fact("r", vec![Value::Int(0)]).unwrap();
    engine.run().unwrap();
    let expected = i64::try_from(MAX_EXPR_DEPTH).unwrap();
    assert_eq!(engine.query("q"), vec![vec![Value::Int(expected)]]);
}

#[test]
fn a_flat_chain_one_past_the_limit_is_rejected() {
    assert_too_deep(&program_with_head(&flat_sum(MAX_EXPR_DEPTH + 1)));
}

#[test]
fn a_huge_flat_chain_is_rejected_without_overflow() {
    assert_too_deep(&program_with_head(&flat_sum(200_000)));
}

#[test]
fn deeply_nested_parentheses_are_rejected_without_overflow() {
    let n = 200_000;
    let head = format!("{}1{}", "(".repeat(n), ")".repeat(n));
    assert_too_deep(&program_with_head(&head));
}

#[test]
fn a_long_prefix_operator_run_is_rejected_without_overflow() {
    assert_too_deep(&program_with_head(&format!("{}1", "- ".repeat(200_000))));
}

#[test]
fn a_hand_built_deep_expression_fails_closed() {
    let mut e = Expr::Lit(Value::Bool(true));
    for _ in 0..MAX_EXPR_DEPTH {
        e = Expr::Unary(UnOp::Not, Box::new(e));
    }
    assert_eq!(e.depth(), MAX_EXPR_DEPTH + 1);
    assert_eq!(e.eval(&HashMap::new()), Err(ExprError::DepthLimit));
    assert_eq!(e.infer(&HashMap::new()), Err(ExprError::DepthLimit));
}

/// The body walk recurses once per clause and nothing bounds the clause count,
/// so this would overflow a test thread's stack without on-demand growth.
#[test]
fn a_very_long_rule_body_runs_without_overflow() {
    let clauses = vec!["if true"; 100_000].join(", ");
    let src = format!("relation r(int); relation q(int); q(1) <-- r(_), {clauses};");
    let mut engine = Engine::from_source(&src).unwrap();
    engine.add_fact("r", vec![Value::Int(0)]).unwrap();
    engine.run().unwrap();
    assert_eq!(engine.query("q"), vec![vec![Value::Int(1)]]);
}
