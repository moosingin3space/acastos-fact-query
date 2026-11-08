//! Engine-level semantics tests that do not need the `ascent!` macro as an
//! oracle: expression-heavy derivation and load-time stratification checks.

use ascent_jit::{Engine, Value};

const DIST_SQ: &str = "relation point(int, int, int);
     relation dist_sq(int, int, int);
     dist_sq(a, b, d) <--
         point(a, ax, ay),
         point(b, bx, by),
         if a != b,
         let dx = bx - ax,
         let dy = by - ay,
         let d = dx * dx + dy * dy;";

fn load_points(engine: &mut Engine) {
    for (a, b, c) in [(1, 0, 0), (2, 3, 4), (3, 1, 1)] {
        engine
            .add_fact("point", vec![Value::Int(a), Value::Int(b), Value::Int(c)])
            .unwrap();
    }
}

/// Arithmetic and comparison expressions in `if`/`let` clauses derive exactly
/// the expected squared distances.
#[test]
fn derives_expression_heavy_rule() {
    let mut engine = Engine::from_source(DIST_SQ).unwrap();
    load_points(&mut engine);
    engine.run().unwrap();

    let mut rows = engine.query("dist_sq");
    rows.sort();
    let expected: Vec<Vec<Value>> = [
        (1, 2, 25),
        (1, 3, 2),
        (2, 1, 25),
        (2, 3, 13),
        (3, 1, 2),
        (3, 2, 13),
    ]
    .into_iter()
    .map(|(a, b, d)| vec![Value::Int(a), Value::Int(b), Value::Int(d)])
    .collect();
    assert_eq!(rows, expected);
}

/// A cycle through negation cannot be stratified and must be rejected when the
/// program is loaded — exactly as the `ascent!` macro rejects it at compile
/// time.
#[test]
fn rejects_negation_cycle() {
    let result = Engine::from_source(
        "relation base(int);
         relation p(int);
         relation r(int);
         p(x) <-- base(x), !r(x);
         r(x) <-- base(x), !p(x);",
    );
    assert!(result.is_err(), "negation cycle should be rejected");
}

/// A non-cyclic stratified program with negation still loads and runs.
#[test]
fn accepts_stratified_negation() {
    let mut engine = Engine::from_source(
        "relation base(int);
         relation p(int);
         relation r(int);
         p(x) <-- base(x), if x > 0;
         r(x) <-- base(x), !p(x);",
    )
    .unwrap();
    for x in [-1, 0, 1, 2] {
        engine.add_fact("base", vec![Value::Int(x)]).unwrap();
    }
    engine.run().unwrap();
    let mut r = engine.query("r");
    r.sort();
    assert_eq!(r, vec![vec![Value::Int(-1)], vec![Value::Int(0)]]);
}
