//! Coverage-guided expression fuzzing: WASM tier vs interpreter oracle.

#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let _ = ascent_jit::fuzz::differential_expr(&mut u);
});
