//! Separate binary so the capped allocator stays here.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{BufReader, ErrorKind};

use parsio::{Read, ReadError, StdReader};

/// Max `read_slice` may reserve on a length alone
const CAP: usize = 4 << 20;

struct Capped;

unsafe impl GlobalAlloc for Capped {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() > CAP {
            return std::ptr::null_mut();
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Capped = Capped;

/// Crafted lengths, peer stalls: EOF, never more than 4MiB asked. 32MiB is CBOR `5a 02 00 00 00`
#[test]
fn crafted_length_reserves_at_most_the_free_window() {
    for (len, sent) in [
        (32usize << 20, 0usize),
        (32 << 20, 1),
        (32 << 20, (32 << 20) / 32 - 1),
        (1 << 30, 0),
        (1 << 30, (4 << 20) - 1),
        (usize::MAX, 0),
    ] {
        let data = vec![7u8; sent];
        let mut reader = StdReader::new(BufReader::new(&data[..]));
        assert!(
            matches!(
                reader.read_slice(len),
                Err(ReadError::IoError(ErrorKind::UnexpectedEof))
            ),
            "len {len}, {sent} bytes sent"
        );
    }
}

/// Proof arrived, rest refused: error, not abort
#[test]
fn refused_reservation_is_an_error() {
    for len in [32usize << 20, 1 << 30] {
        let data = vec![7u8; (len / 32).min(4 << 20)];
        let mut reader = StdReader::new(BufReader::new(&data[..]));
        assert!(
            matches!(
                reader.read_slice(len),
                Err(ReadError::IoError(ErrorKind::OutOfMemory))
            ),
            "len {len}"
        );
    }
}
