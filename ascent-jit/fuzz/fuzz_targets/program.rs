//! Coverage-guided program fuzzing: the semi-naïve evaluator vs the naïve
//! reference loop over random stratifiable programs and facts.

#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let _ = ascent_jit::fuzz::differential_seminaive(&mut u);
});
