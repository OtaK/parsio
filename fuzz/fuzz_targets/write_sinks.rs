#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

use parsio_fuzz::{WriteInput, fuzz_write_sinks};

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = WriteInput::arbitrary(&mut Unstructured::new(data)) {
        fuzz_write_sinks(&input);
    }
});
