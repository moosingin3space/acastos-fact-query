//! Namespaced relation names (`policy::grant`, `policy::deny`) parse, and a
//! default-deny `allowed/1` composition evaluates correctly: a privilege exists
//! only if explicitly granted and not denied. Namespace *enforcement* (who may
//! write which namespace) is a host concern; here we only verify the engine can
//! express and evaluate the rule.

use ascent_jit::{Engine, Value};

fn int(n: i64) -> Value {
    Value::Int(n)
}

const DEFAULT_DENY: &str = "relation request(int);
     relation policy::grant(int);
     relation policy::deny(int);
     relation user::deny(int);
     relation allowed(int);
     allowed(x) <--
         request(x),
         policy::grant(x),
         !policy::deny(x),
         !user::deny(x);";

#[test]
fn default_deny_allows_only_explicit_grants() {
    let mut engine = Engine::from_source(DEFAULT_DENY).unwrap();

    // Four requested actions.
    for x in 1..=4 {
        engine.add_fact("request", vec![int(x)]).unwrap();
    }
    // 1, 2, 3 are granted; 4 is NOT (default-deny applies to it).
    for x in 1..=3 {
        engine.add_fact("policy::grant", vec![int(x)]).unwrap();
    }
    // The policy floor denies 2; a user rule denies 3.
    engine.add_fact("policy::deny", vec![int(2)]).unwrap();
    engine.add_fact("user::deny", vec![int(3)]).unwrap();

    engine.run().unwrap();

    // Only 1 survives: granted, not policy-denied, not user-denied.
    // 2 policy-denied; 3 user-denied; 4 never granted (the default).
    assert_eq!(engine.query("allowed"), vec![vec![int(1)]]);
}

#[test]
fn a_lone_colon_is_still_a_lex_error() {
    assert!(Engine::from_source("relation a:b(int);").is_err());
}
