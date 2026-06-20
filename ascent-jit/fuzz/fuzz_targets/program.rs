//! Coverage-guided program fuzzing: WASM-backed engine vs interpreter-backed
//! engine over random stratifiable programs and facts.

#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let _ = ascent_jit::fuzz::differential_program(&mut u);
});
