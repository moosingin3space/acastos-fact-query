//! Coverage-guided expression fuzzing: the interpreter is type-sound on
//! random well-typed expressions.

#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let _ = ascent_jit::fuzz::expr_soundness(&mut u);
});
