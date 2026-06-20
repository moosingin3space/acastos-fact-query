//! Differential tests: every major Ascent surface is run through both the real
//! `ascent!` proc macro and the WASM-powered `ascent-jit` runtime, and their
//! derived relations are asserted equal as sets.
//!
//! This is the load-bearing correctness check for the crate: the interpreter is
//! only as trustworthy as its agreement with the engine it is imitating.

// `ascent`'s generated program is, by its public API, default-constructed and
// then populated field by field.
#![expect(
    clippy::field_reassign_with_default,
    reason = "this is the documented way to load facts into an `ascent!` program"
)]
// The `ascent!` proc macro expands to code with call-site spans, so its
// internal underscore-prefixed bindings surface as local lints.
#![expect(
    clippy::used_underscore_binding,
    reason = "originates inside the `ascent!` macro expansion, not our code"
)]

use std::collections::BTreeSet;

use ascent_jit::{Engine, Value};

/// A schema-agnostic cell, so tuples from the macro (statically typed) and from
/// the runtime (dynamically typed) can be compared in one representation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Cell {
    Int(i64),
    Bool(bool),
    Str(String),
}

type Rows = BTreeSet<Vec<Cell>>;

/// Collects a runtime relation into the comparison representation.
fn jit_rows(engine: &Engine, relation: &str) -> Rows {
    engine
        .query(relation)
        .into_iter()
        .map(|tuple| {
            tuple
                .into_iter()
                .map(|v| match v {
                    Value::Int(i) => Cell::Int(i),
                    Value::Bool(b) => Cell::Bool(b),
                    Value::Sym(s) => Cell::Str(engine.resolve(s).expect("interned").to_owned()),
                })
                .collect()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Surface 1: plain Datalog — transitive closure.
// ---------------------------------------------------------------------------
mod transitive_closure {
    use super::{Cell, Engine, Rows, Value, jit_rows};

    ascent::ascent! {
        relation edge(i32, i32);
        relation path(i32, i32);
        path(x, y) <-- edge(x, y);
        path(x, z) <-- edge(x, y), path(y, z);
    }

    #[test]
    fn matches_macro() {
        let edges = [(1, 2), (2, 3), (3, 4), (2, 5)];

        let mut macro_prog = AscentProgram::default();
        macro_prog.edge = edges.to_vec();
        macro_prog.run();
        let macro_rows: Rows = macro_prog
            .path
            .iter()
            .map(|&(a, b)| vec![Cell::Int(i64::from(a)), Cell::Int(i64::from(b))])
            .collect();

        let mut engine = Engine::from_source(
            "relation edge(int, int);
             relation path(int, int);
             path(x, y) <-- edge(x, y);
             path(x, z) <-- edge(x, y), path(y, z);",
        )
        .unwrap();
        for (a, b) in edges {
            engine
                .add_fact(
                    "edge",
                    vec![Value::Int(i64::from(a)), Value::Int(i64::from(b))],
                )
                .unwrap();
        }
        engine.run().unwrap();

        assert_eq!(jit_rows(&engine, "path"), macro_rows);
    }
}

// ---------------------------------------------------------------------------
// Surface 2: conditions + stratified negation — even / odd.
// ---------------------------------------------------------------------------
mod conditions_and_negation {
    use super::{Cell, Engine, Rows, Value, jit_rows};

    ascent::ascent! {
        relation number(i32);
        relation even(i32);
        relation odd(i32);
        even(x) <-- number(x), if x % 2 == 0;
        odd(x) <-- number(x), !even(x);
    }

    #[test]
    fn matches_macro() {
        let mut macro_prog = AscentProgram::default();
        macro_prog.number = (1..=10).map(|n| (n,)).collect();
        macro_prog.run();
        let macro_even: Rows = macro_prog
            .even
            .iter()
            .map(|&(x,)| vec![Cell::Int(i64::from(x))])
            .collect();
        let macro_odd: Rows = macro_prog
            .odd
            .iter()
            .map(|&(x,)| vec![Cell::Int(i64::from(x))])
            .collect();

        let mut engine = Engine::from_source(
            "relation number(int);
             relation even(int);
             relation odd(int);
             even(x) <-- number(x), if x % 2 == 0;
             odd(x) <-- number(x), !even(x);",
        )
        .unwrap();
        for n in 1..=10 {
            engine.add_fact("number", vec![Value::Int(n)]).unwrap();
        }
        engine.run().unwrap();

        assert_eq!(jit_rows(&engine, "even"), macro_even);
        assert_eq!(jit_rows(&engine, "odd"), macro_odd);
    }
}

// ---------------------------------------------------------------------------
// Surface 3: `let` bindings + arithmetic — squared distances.
// ---------------------------------------------------------------------------
mod let_bindings {
    use super::{Cell, Engine, Rows, Value, jit_rows};

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

    #[test]
    fn matches_macro() {
        let points = [(1, 0, 0), (2, 3, 4), (3, 1, 1)];

        let mut macro_prog = AscentProgram::default();
        macro_prog.point = points.to_vec();
        macro_prog.run();
        let macro_rows: Rows = macro_prog
            .dist_sq
            .iter()
            .map(|&(a, b, d)| {
                vec![
                    Cell::Int(i64::from(a)),
                    Cell::Int(i64::from(b)),
                    Cell::Int(d),
                ]
            })
            .collect();

        let mut engine = Engine::from_source(
            "relation point(int, int, int);
             relation dist_sq(int, int, int);
             dist_sq(a, b, d) <--
                 point(a, ax, ay),
                 point(b, bx, by),
                 if a != b,
                 let dx = bx - ax,
                 let dy = by - ay,
                 let d = dx * dx + dy * dy;",
        )
        .unwrap();
        for (a, b, c) in points {
            engine
                .add_fact(
                    "point",
                    vec![
                        Value::Int(i64::from(a)),
                        Value::Int(i64::from(b)),
                        Value::Int(i64::from(c)),
                    ],
                )
                .unwrap();
        }
        engine.run().unwrap();

        assert_eq!(jit_rows(&engine, "dist_sq"), macro_rows);
    }
}

// ---------------------------------------------------------------------------
// Surface 4: aggregation — min / max / sum / count.
// ---------------------------------------------------------------------------
mod aggregation {
    use super::{Cell, Engine, Rows, Value, jit_rows};
    use ascent::aggregators::{count, max, min, sum};

    ascent::ascent! {
        relation grade(u32, u32);
        relation stats(u32, u32, u32, usize);
        stats(mn as u32, mx as u32, s as u32, cnt) <--
            agg mn = min(g) in grade(_, g),
            agg mx = max(g) in grade(_, g),
            agg s = sum(g) in grade(_, g),
            agg cnt = count() in grade(_, _);
    }

    #[test]
    fn matches_macro() {
        let grades = [(1, 85), (2, 92), (3, 78), (4, 95), (5, 88)];

        let mut macro_prog = AscentProgram::default();
        macro_prog.grade = grades.to_vec();
        macro_prog.run();
        let macro_rows: Rows = macro_prog
            .stats
            .iter()
            .map(|&(mn, mx, s, cnt)| {
                vec![
                    Cell::Int(i64::from(mn)),
                    Cell::Int(i64::from(mx)),
                    Cell::Int(i64::from(s)),
                    Cell::Int(i64::try_from(cnt).unwrap()),
                ]
            })
            .collect();

        let mut engine = Engine::from_source(
            "relation grade(int, int);
             relation stats(int, int, int, int);
             stats(mn, mx, s, cnt) <--
                 agg mn = min(g) in grade(_, g),
                 agg mx = max(g) in grade(_, g),
                 agg s = sum(g) in grade(_, g),
                 agg cnt = count() in grade(_, _);",
        )
        .unwrap();
        for (st, g) in grades {
            engine
                .add_fact(
                    "grade",
                    vec![Value::Int(i64::from(st)), Value::Int(i64::from(g))],
                )
                .unwrap();
        }
        engine.run().unwrap();

        assert_eq!(jit_rows(&engine, "stats"), macro_rows);
    }
}

// ---------------------------------------------------------------------------
// Surface 5: lattices — shortest path with `Dual` (keep the minimum).
// ---------------------------------------------------------------------------
mod lattice_shortest_path {
    use super::{Cell, Engine, Rows, Value, jit_rows};
    use ascent::Dual;

    ascent::ascent! {
        relation edge(i32, i32, u32);
        lattice shortest_path(i32, i32, Dual<u32>);
        shortest_path(x, y, Dual(*w)) <-- edge(x, y, w);
        shortest_path(x, z, Dual(w + l)) <--
            edge(x, y, w),
            shortest_path(y, z, ?Dual(l));
    }

    #[test]
    fn matches_macro() {
        let edges = [(0, 1, 4), (0, 2, 1), (2, 1, 2), (1, 3, 1)];

        let mut macro_prog = AscentProgram::default();
        macro_prog.edge = edges.to_vec();
        macro_prog.run();
        let macro_rows: Rows = macro_prog
            .shortest_path
            .iter()
            .map(|&(x, y, Dual(cost))| {
                vec![
                    Cell::Int(i64::from(x)),
                    Cell::Int(i64::from(y)),
                    Cell::Int(i64::from(cost)),
                ]
            })
            .collect();

        let mut engine = Engine::from_source(
            "relation edge(int, int, int);
             lattice shortest_path(int, int, dual int);
             shortest_path(x, y, Dual(w)) <-- edge(x, y, w);
             shortest_path(x, z, Dual(w + l)) <--
                 edge(x, y, w),
                 shortest_path(y, z, ?Dual(l));",
        )
        .unwrap();
        for (x, y, w) in edges {
            engine
                .add_fact(
                    "edge",
                    vec![
                        Value::Int(i64::from(x)),
                        Value::Int(i64::from(y)),
                        Value::Int(i64::from(w)),
                    ],
                )
                .unwrap();
        }
        engine.run().unwrap();

        assert_eq!(jit_rows(&engine, "shortest_path"), macro_rows);
    }
}

// ---------------------------------------------------------------------------
// Surface 6: symbols (interned strings) through joins — transitive closure.
// ---------------------------------------------------------------------------
mod symbols {
    use super::{Cell, Engine, Rows, Value, jit_rows};

    ascent::ascent! {
        relation compiler(&'static str, &'static str);
        relation can(&'static str, &'static str);
        can(a, b) <-- compiler(a, b);
        can(a, c) <-- compiler(a, b), can(b, c);
    }

    #[test]
    fn matches_macro() {
        let links = [("rust", "wasm"), ("wasm", "native"), ("native", "exe")];

        let mut macro_prog = AscentProgram::default();
        macro_prog.compiler = links.to_vec();
        macro_prog.run();
        let macro_rows: Rows = macro_prog
            .can
            .iter()
            .map(|&(a, b)| vec![Cell::Str(a.to_owned()), Cell::Str(b.to_owned())])
            .collect();

        let mut engine = Engine::from_source(
            "relation compiler(sym, sym);
             relation can(sym, sym);
             can(a, b) <-- compiler(a, b);
             can(a, c) <-- compiler(a, b), can(b, c);",
        )
        .unwrap();
        for (a, b) in links {
            let a = engine.intern(a);
            let b = engine.intern(b);
            engine
                .add_fact("compiler", vec![Value::Sym(a), Value::Sym(b)])
                .unwrap();
        }
        engine.run().unwrap();

        assert_eq!(jit_rows(&engine, "can"), macro_rows);
    }
}
