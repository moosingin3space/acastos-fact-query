//! Coverage-guided fact fuzzing of transitive closure against the `ascent!`
//! macro. The rules are fixed (compile-time); only the EDB `edge` facts vary.

#![no_main]

use std::collections::BTreeSet;

use ascent_jit::{Engine, Value};
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

ascent::ascent! {
    relation edge(i32, i32);
    relation path(i32, i32);
    path(x, y) <-- edge(x, y);
    path(x, z) <-- edge(x, y), path(y, z);
}

const SRC: &str = "relation edge(int, int);
     relation path(int, int);
     path(x, y) <-- edge(x, y);
     path(x, z) <-- edge(x, y), path(y, z);";

fn int(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        other => panic!("expected Int, got {other:?}"),
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok(pairs) = ascent_jit::fuzz::arb_pairs(&mut u, 16, 0, 6) else {
        return;
    };

    let mut m = AscentProgram::default();
    m.edge = pairs
        .iter()
        .map(|&(a, b)| (i32::try_from(a).unwrap(), i32::try_from(b).unwrap()))
        .collect();
    m.run();
    let macro_set: BTreeSet<(i64, i64)> =
        m.path.iter().map(|&(a, b)| (i64::from(a), i64::from(b))).collect();

    let mut e = Engine::from_source(SRC).unwrap();
    for &(a, b) in &pairs {
        e.add_fact("edge", vec![Value::Int(a), Value::Int(b)]).unwrap();
    }
    e.run().unwrap();
    let jit_set: BTreeSet<(i64, i64)> =
        e.query("path").iter().map(|t| (int(&t[0]), int(&t[1]))).collect();

    assert_eq!(macro_set, jit_set, "transitive closure diverged on {pairs:?}");
});
