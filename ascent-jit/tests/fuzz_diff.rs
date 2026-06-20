//! Differential *fuzzing* (as opposed to the fixed-fixture differential tests
//! in `differential.rs`). Runs under `cargo test`/`just check` whenever the
//! `arbitrary` feature is on; the same generators drive the `cargo-fuzz`
//! targets for longer, coverage-guided runs.
//!
//! - `*_oracle` tests: WASM tier vs the interpreter oracle, over random
//!   expressions and random programs.
//! - `macro_*` tests: random EDB facts over the fixed fixtures, compared to the
//!   real `ascent!` macro (the rules are compile-time, so only the facts vary).

#![cfg(feature = "arbitrary")]
// See `differential.rs`: these originate from `ascent`'s public API and macro
// expansion, not our code.
#![expect(
    clippy::field_reassign_with_default,
    reason = "documented way to load facts into an `ascent!` program"
)]
#![expect(
    clippy::used_underscore_binding,
    reason = "originates inside the `ascent!` macro expansion"
)]

use std::collections::BTreeSet;

use ascent_jit::{Engine, Value, fuzz};

fn int(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        other => panic!("expected Int, got {other:?}"),
    }
}

#[test]
fn expr_oracle() {
    arbtest::arbtest(fuzz::differential_expr);
}

#[test]
fn program_oracle() {
    arbtest::arbtest(fuzz::differential_program);
}

// --- macro fact-fuzzing: plain Datalog (transitive closure) ----------------
mod tc {
    use super::{BTreeSet, Engine, Value, int};

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

    pub fn check(pairs: &[(i64, i64)]) {
        let mut m = AscentProgram::default();
        m.edge = pairs
            .iter()
            .map(|&(a, b)| (i32::try_from(a).unwrap(), i32::try_from(b).unwrap()))
            .collect();
        m.run();
        let macro_set: BTreeSet<(i64, i64)> = m
            .path
            .iter()
            .map(|&(a, b)| (i64::from(a), i64::from(b)))
            .collect();

        let mut e = Engine::from_source(SRC).unwrap();
        for &(a, b) in pairs {
            e.add_fact("edge", vec![Value::Int(a), Value::Int(b)])
                .unwrap();
        }
        e.run().unwrap();
        let jit_set: BTreeSet<(i64, i64)> = e
            .query("path")
            .iter()
            .map(|t| (int(&t[0]), int(&t[1])))
            .collect();

        assert_eq!(
            macro_set, jit_set,
            "transitive closure diverged on {pairs:?}"
        );
    }
}

#[test]
fn macro_transitive_closure() {
    arbtest::arbtest(|u| {
        tc::check(&fuzz::arb_pairs(u, 12, 0, 5)?);
        Ok(())
    });
}

// --- macro fact-fuzzing: conditions + negation (even / odd) ----------------
mod even_odd {
    use super::{BTreeSet, Engine, Value, int};

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

    pub fn check(nums: &[i64]) {
        let mut m = AscentProgram::default();
        m.number = nums.iter().map(|&n| (i32::try_from(n).unwrap(),)).collect();
        m.run();
        let macro_odd: BTreeSet<i64> = m.odd.iter().map(|&(x,)| i64::from(x)).collect();

        let mut e = Engine::from_source(SRC).unwrap();
        for &n in nums {
            e.add_fact("number", vec![Value::Int(n)]).unwrap();
        }
        e.run().unwrap();
        let jit_odd: BTreeSet<i64> = e.query("odd").iter().map(|t| int(&t[0])).collect();

        assert_eq!(macro_odd, jit_odd, "odd diverged on {nums:?}");
    }
}

#[test]
fn macro_even_odd() {
    arbtest::arbtest(|u| {
        even_odd::check(&fuzz::arb_singletons(u, 16, -8, 8)?);
        Ok(())
    });
}

// --- macro fact-fuzzing: arithmetic in head (squared distances) ------------
mod dist {
    use super::{BTreeSet, Engine, Value, int};

    ascent::ascent! {
        relation point(i32, i32, i32);
        relation dist_sq(i32, i32, i64);
        dist_sq(a, b, d) <--
            point(a, ax, ay),
            point(b, bx, by),
            if a != b,
            let dx = i64::from(*bx - *ax),
            let dy = i64::from(*by - *ay),
            let d = dx * dx + dy * dy;
    }

    const SRC: &str = "relation point(int, int, int);
         relation dist_sq(int, int, int);
         dist_sq(a, b, d) <--
             point(a, ax, ay),
             point(b, bx, by),
             if a != b,
             let dx = bx - ax,
             let dy = by - ay,
             let d = dx * dx + dy * dy;";

    pub fn check(points: &[(i64, i64, i64)]) {
        let mut m = AscentProgram::default();
        m.point = points
            .iter()
            .map(|&(a, b, c)| {
                (
                    i32::try_from(a).unwrap(),
                    i32::try_from(b).unwrap(),
                    i32::try_from(c).unwrap(),
                )
            })
            .collect();
        m.run();
        let macro_set: BTreeSet<(i64, i64, i64)> = m
            .dist_sq
            .iter()
            .map(|&(a, b, d)| (i64::from(a), i64::from(b), d))
            .collect();

        let mut e = Engine::from_source(SRC).unwrap();
        for &(a, b, c) in points {
            e.add_fact("point", vec![Value::Int(a), Value::Int(b), Value::Int(c)])
                .unwrap();
        }
        e.run().unwrap();
        let jit_set: BTreeSet<(i64, i64, i64)> = e
            .query("dist_sq")
            .iter()
            .map(|t| (int(&t[0]), int(&t[1]), int(&t[2])))
            .collect();

        assert_eq!(macro_set, jit_set, "dist_sq diverged on {points:?}");
    }
}

#[test]
fn macro_dist_sq() {
    arbtest::arbtest(|u| {
        dist::check(&fuzz::arb_triples(u, 8, -6, 6)?);
        Ok(())
    });
}

// --- macro fact-fuzzing: lattice (shortest path with Dual) -----------------
mod sp {
    use super::{BTreeSet, Engine, Value, int};
    use ascent::Dual;

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

    pub fn check(edges: &[(i64, i64, i64)]) {
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
        for &(x, y, w) in edges {
            e.add_fact("edge", vec![Value::Int(x), Value::Int(y), Value::Int(w)])
                .unwrap();
        }
        e.run().unwrap();
        let jit_set: BTreeSet<(i64, i64, i64)> = e
            .query("shortest_path")
            .iter()
            .map(|t| (int(&t[0]), int(&t[1]), int(&t[2])))
            .collect();

        assert_eq!(macro_set, jit_set, "shortest_path diverged on {edges:?}");
    }
}

#[test]
fn macro_shortest_path() {
    arbtest::arbtest(|u| {
        // Non-negative weights only: the macro's column is `u32`.
        sp::check(&fuzz::arb_triples(u, 10, 0, 5)?);
        Ok(())
    });
}
