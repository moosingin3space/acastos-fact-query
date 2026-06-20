//! Coverage-guided fact fuzzing of the lattice surface (shortest path with
//! `Dual`) against the `ascent!` macro. Only the weighted `edge` EDB varies.

#![no_main]

use std::collections::BTreeSet;

use ascent::Dual;
use ascent_jit::{Engine, Value};
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

ascent::ascent! {
    relation edge(i32, i32, u32);
    lattice shortest_path(i32, i32, Dual<u32>);
    shortest_path(x, y, Dual(*w)) <-- edge(x, y, w);
    shortest_path(x, z, Dual(w + l)) <--
        edge(x, y, w),
        shortest_path(y, z, ?Dual(l));
}

const SRC: &str = "relation edge(int, int, int);
     lattice shortest_path(int, int, dual int);
     shortest_path(x, y, Dual(w)) <-- edge(x, y, w);
     shortest_path(x, z, Dual(w + l)) <--
         edge(x, y, w),
         shortest_path(y, z, ?Dual(l));";

fn int(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        other => panic!("expected Int, got {other:?}"),
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    // Non-negative weights only: the macro's weight column is `u32`.
    let Ok(edges) = ascent_jit::fuzz::arb_triples(&mut u, 12, 0, 6) else {
        return;
    };

    let mut m = AscentProgram::default();
    m.edge = edges
        .iter()
        .map(|&(x, y, w)| {
            (
                i32::try_from(x).unwrap(),
                i32::try_from(y).unwrap(),
                u32::try_from(w).unwrap(),
            )
        })
        .collect();
    m.run();
    let macro_set: BTreeSet<(i64, i64, i64)> = m
        .shortest_path
        .iter()
        .map(|&(x, y, Dual(c))| (i64::from(x), i64::from(y), i64::from(c)))
        .collect();

    let mut e = Engine::from_source(SRC).unwrap();
    for &(x, y, w) in &edges {
        e.add_fact("edge", vec![Value::Int(x), Value::Int(y), Value::Int(w)]).unwrap();
    }
    e.run().unwrap();
    let jit_set: BTreeSet<(i64, i64, i64)> = e
        .query("shortest_path")
        .iter()
        .map(|t| (int(&t[0]), int(&t[1]), int(&t[2])))
        .collect();

    assert_eq!(macro_set, jit_set, "shortest_path diverged on {edges:?}");
});
