//! Coverage-guided fact fuzzing of conditions + stratified negation
//! (even/odd) against the `ascent!` macro. Only the `number` EDB varies.

#![no_main]

use std::collections::BTreeSet;

use ascent_jit::{Engine, Value};
use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

ascent::ascent! {
    relation number(i32);
    relation even(i32);
    relation odd(i32);
    even(x) <-- number(x), if x % 2 == 0;
    odd(x) <-- number(x), !even(x);
}

const SRC: &str = "relation number(int);
     relation even(int);
     relation odd(int);
     even(x) <-- number(x), if x % 2 == 0;
     odd(x) <-- number(x), !even(x);";

fn int(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        other => panic!("expected Int, got {other:?}"),
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let Ok(nums) = ascent_jit::fuzz::arb_singletons(&mut u, 20, -10, 10) else {
        return;
    };

    let mut m = AscentProgram::default();
    m.number = nums.iter().map(|&n| (i32::try_from(n).unwrap(),)).collect();
    m.run();
    let macro_even: BTreeSet<i64> = m.even.iter().map(|&(x,)| i64::from(x)).collect();
    let macro_odd: BTreeSet<i64> = m.odd.iter().map(|&(x,)| i64::from(x)).collect();

    let mut e = Engine::from_source(SRC).unwrap();
    for &n in &nums {
        e.add_fact("number", vec![Value::Int(n)]).unwrap();
    }
    e.run().unwrap();
    let jit_even: BTreeSet<i64> = e.query("even").iter().map(|t| int(&t[0])).collect();
    let jit_odd: BTreeSet<i64> = e.query("odd").iter().map(|t| int(&t[0])).collect();

    assert_eq!(macro_even, jit_even, "even diverged on {nums:?}");
    assert_eq!(macro_odd, jit_odd, "odd diverged on {nums:?}");
});
