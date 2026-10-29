#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

use parsio_fuzz::{DepthInput, fuzz_depth_accounting};

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = DepthInput::arbitrary(&mut Unstructured::new(data)) {
        fuzz_depth_accounting(&input);
    }
});
