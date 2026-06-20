//! Engine-level semantics tests that do not need the `ascent!` macro as an
//! oracle: WASM-vs-interpreter agreement and load-time stratification checks.

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

/// The WASM expression tier must agree with the pure interpreter oracle on a
/// program whose rules lean on arithmetic and comparison expressions.
#[test]
fn wasm_agrees_with_interpreter() {
    let mut wasm = Engine::from_source(DIST_SQ).unwrap();
    load_points(&mut wasm);
    wasm.run().unwrap();

    let mut interp = Engine::from_source_interpreted(DIST_SQ).unwrap();
    load_points(&mut interp);
    interp.run().unwrap();

    let mut wasm_rows = wasm.query("dist_sq");
    let mut interp_rows = interp.query("dist_sq");
    wasm_rows.sort();
    interp_rows.sort();
    assert_eq!(wasm_rows, interp_rows);
    assert!(!wasm_rows.is_empty());
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
