//! Q2 — expression cost.
//!
//! Two views of the per-expression cost of the tree-walk interpreter:
//!
//! * `expr_heavy_run` — an engine workload dominated by expression evaluation
//!   with no join: one relation of `n` ints, a rule with chained `let` bindings
//!   and `if` conditions doing arithmetic. Only the fixed-point run is timed.
//! * `expr_single_call` — a microbenchmark of one `Expr::eval` call on a
//!   hand-built expression with a prepared environment.

use std::collections::HashMap;
use std::hint::black_box;
use std::time::Duration;

use ascent_jit::expr::{BinOp, Expr};
use ascent_jit::{Engine, Symbol, Value};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

/// One relation of ints; the rule does several arithmetic `let`s and `if`
/// filters per tuple and derives into `out`, so expression evaluation dominates
/// and join work is a single relation scan.
const EXPR_SRC: &str = "relation num(int);
relation out(int);
out(z) <-- num(x), let a = x * x, let b = a + x, if b > 5, if b % 2 == 0, let z = b - a, if z < 1000000;";

/// Number of `num` tuples for the expression-heavy engine workload.
const N: i64 = 2000;

fn load_engine() -> Engine {
    let mut engine = Engine::from_source(EXPR_SRC).expect("program loads");
    for x in 0..N {
        engine
            .add_fact("num", vec![Value::Int(x)])
            .expect("num is a declared unary relation");
    }
    engine
}

fn bench_expr_heavy(c: &mut Criterion) {
    let mut group = c.benchmark_group("expr_heavy_run");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(3));

    group.bench_function("interpreted", |b| {
        b.iter_batched(
            load_engine,
            |mut engine| {
                engine.run().expect("run reaches a fixed point");
                black_box(engine.query("out").len())
            },
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

/// Hand-builds the expression `x * x + 3 > 10` over a single variable, plus the
/// environment binding `x = 7`. The variable symbol is arbitrary but must match
/// between the expression and the environment.
fn single_call_fixture() -> (Expr, HashMap<Symbol, Value>) {
    let x = Symbol(0);
    let expr = Expr::Binary(
        BinOp::Gt,
        Box::new(Expr::Binary(
            BinOp::Add,
            Box::new(Expr::Binary(
                BinOp::Mul,
                Box::new(Expr::Var(x)),
                Box::new(Expr::Var(x)),
            )),
            Box::new(Expr::Lit(Value::Int(3))),
        )),
        Box::new(Expr::Lit(Value::Int(10))),
    );
    let mut env = HashMap::new();
    env.insert(x, Value::Int(7));
    (expr, env)
}

fn bench_single_call(c: &mut Criterion) {
    let (expr, env) = single_call_fixture();

    let mut group = c.benchmark_group("expr_single_call");

    group.bench_function("interpreted", |b| {
        b.iter(|| black_box(black_box(&expr).eval(black_box(&env)).expect("evaluates")));
    });

    group.finish();
}

criterion_group!(benches, bench_expr_heavy, bench_single_call);
criterion_main!(benches);
