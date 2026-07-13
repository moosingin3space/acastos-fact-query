//! Q3 — queries grain.
//!
//! Materializes a small fact base (three relations, a few hundred tuples) and
//! benchmarks a read-only conjunctive query — a three-atom join with an `if`
//! condition and a cardinality cap — through `Engine::evaluate_query`, comparing
//! the wasmtime and interpreted expression tiers. The fact base is built once;
//! only the query evaluation is timed.

use std::hint::black_box;

use ascent_jit::ir::BodyClause;
use ascent_jit::{Engine, Value};
use criterion::{Criterion, criterion_group, criterion_main};

/// Schema for the fact base. No rules: the queries grain evaluates directly
/// against the materialized relations.
const SRC: &str = "relation usr(int, int);
relation dept(int, int);
relation active(int);";

/// A three-atom conjunctive join with a filter: users in a department whose
/// budget clears a threshold, restricted to active users.
const QUERY: &str = "(u, b) <-- usr(u, d), dept(d, b), active(u), if b > 500";

const USERS: i64 = 300;
const DEPTS: i64 = 30;
/// No-truncation cap: comfortably above the solution count, so the full join is
/// measured rather than an early cut-off.
const MAX: usize = 100_000;

fn load_engine(build: fn(&str) -> Result<Engine, ascent_jit::Error>) -> Engine {
    let mut engine = build(SRC).expect("program loads");
    for dept in 0..DEPTS {
        // Budgets fan from 0..3000 so roughly five sixths clear the threshold.
        engine
            .add_fact("dept", vec![Value::Int(dept), Value::Int(dept * 100)])
            .expect("dept is a declared binary relation");
    }
    for u in 0..USERS {
        engine
            .add_fact("usr", vec![Value::Int(u), Value::Int(u % DEPTS)])
            .expect("usr is a declared binary relation");
        if u % 2 == 0 {
            engine
                .add_fact("active", vec![Value::Int(u)])
                .expect("active is a declared unary relation");
        }
    }
    engine.run().expect("run reaches a fixed point");
    engine
}

fn bench_backend(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &str,
    build: fn(&str) -> Result<Engine, ascent_jit::Error>,
) {
    let mut engine = load_engine(build);
    let (outputs, body): (Vec<_>, Vec<BodyClause>) =
        engine.parse_query_parts(QUERY).expect("query parses");
    engine.check_query(&body).expect("query form-checks");

    group.bench_function(name, |b| {
        b.iter(|| {
            let out = engine
                .evaluate_query(&outputs, &body, MAX)
                .expect("query evaluates");
            black_box(out.solutions.len())
        });
    });
}

fn bench_queries(c: &mut Criterion) {
    let mut group = c.benchmark_group("queries_conjunctive");
    bench_backend(&mut group, "wasmtime", Engine::from_source);
    bench_backend(&mut group, "interpreted", Engine::from_source_interpreted);
    group.finish();
}

criterion_group!(benches, bench_queries);
criterion_main!(benches);
