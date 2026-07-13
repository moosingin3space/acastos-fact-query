//! Q1 — join scaling / naïve-core cost.
//!
//! Transitive closure (`path(x, z) <-- edge(x, y), path(y, z)` plus the base
//! rule) over a chain graph of `n` edges, timed at several sizes. Three engines
//! run on identical inputs: the wasmtime expression tier, the pure interpreter
//! tier, and the monomorphized `ascent!` macro as the baseline. Only the
//! fixed-point run is timed; engine construction, fact loading, and (for the
//! wasm tier) module compilation happen in the untimed setup, so the numbers
//! isolate the relational core's join cost and its scaling curve.

use std::hint::black_box;
use std::time::Duration;

use ascent_jit::{Engine, Value};
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};

/// The `ascent!` macro baseline lives in its own module: the generated struct is
/// `pub`, so nesting it privately keeps it out of the crate's public API surface
/// (where `missing_docs` would fire on the generated associated functions).
mod macro_tc {
    ascent::ascent! {
        relation edge(i64, i64);
        relation path(i64, i64);
        path(x, y) <-- edge(x, y);
        path(x, z) <-- edge(x, y), path(y, z);
    }

    pub use AscentProgram as Program;
}

const SRC: &str = "relation edge(int, int);
relation path(int, int);
path(x, y) <-- edge(x, y);
path(x, z) <-- edge(x, y), path(y, z);";

/// The sizes to sweep: a chain of `n` edges. The naïve core is roughly O(n^3.6)
/// here, so the top size dominates the wall-clock budget.
const SIZES: [i64; 4] = [8, 16, 32, 64];

fn chain(n: i64) -> Vec<(i64, i64)> {
    (0..n).map(|i| (i, i + 1)).collect()
}

/// Builds a loaded-but-not-run engine over the given expression backend.
fn load_engine(
    build: fn(&str) -> Result<Engine, ascent_jit::Error>,
    edges: &[(i64, i64)],
) -> Engine {
    let mut engine = build(SRC).expect("program loads");
    for &(a, b) in edges {
        engine
            .add_fact("edge", vec![Value::Int(a), Value::Int(b)])
            .expect("edge is a declared binary relation");
    }
    engine
}

fn bench_joins(c: &mut Criterion) {
    let mut group = c.benchmark_group("joins_transitive_closure");
    group.sample_size(10);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(6));

    for n in SIZES {
        let edges = chain(n);

        group.bench_with_input(BenchmarkId::new("wasmtime", n), &edges, |b, edges| {
            b.iter_batched(
                || load_engine(Engine::from_source, edges),
                |mut engine| {
                    engine.run().expect("run reaches a fixed point");
                    black_box(engine.query("path").len())
                },
                BatchSize::PerIteration,
            );
        });

        group.bench_with_input(BenchmarkId::new("interpreted", n), &edges, |b, edges| {
            b.iter_batched(
                || load_engine(Engine::from_source_interpreted, edges),
                |mut engine| {
                    engine.run().expect("run reaches a fixed point");
                    black_box(engine.query("path").len())
                },
                BatchSize::PerIteration,
            );
        });

        group.bench_with_input(BenchmarkId::new("ascent_macro", n), &edges, |b, edges| {
            b.iter_batched(
                || {
                    let mut prog = macro_tc::Program::default();
                    prog.edge.extend_from_slice(edges);
                    prog
                },
                |mut prog| {
                    prog.run();
                    black_box(prog.path.len())
                },
                BatchSize::PerIteration,
            );
        });
    }

    group.finish();
}

criterion_group!(benches, bench_joins);
criterion_main!(benches);
