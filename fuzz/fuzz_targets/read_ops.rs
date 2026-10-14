#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

use parsio_fuzz::{ReadInput, fuzz_read_ops, reset_allocated};

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = ReadInput::arbitrary(&mut Unstructured::new(data)) {
        reset_allocated();
        fuzz_read_ops(&input);
    }
});
