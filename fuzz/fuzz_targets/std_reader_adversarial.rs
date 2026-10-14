#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;

use parsio_fuzz::{HostileInput, fuzz_std_reader_adversarial};

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = HostileInput::arbitrary(&mut Unstructured::new(data)) {
        fuzz_std_reader_adversarial(&input.read, &input.schedule);
    }
});
