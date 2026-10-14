//! Fixtures shared by both benchmark engines.
//!
//! criterion and gungraun must measure the *same* work, so the data, the needles, the reader
//! constructors and the drain loop live here; the two bench files only enumerate cells over them.

#![allow(dead_code)] // each engine uses a different subset of this module

use std::borrow::Cow;
use std::io::BufReader;

use parsio::{Read, StdReader};

/// Single-byte delimiter.
pub const ONE: &[u8] = b",";
/// Multi-byte delimiter, for the straddle and seam paths.
pub const TWO: &[u8] = b"\r\n";
/// Never present, for the "scan the whole stream" path.
pub const ABSENT: &[u8] = b"no-such-needle";

/// A stream of `count` records, each `field` bytes of `a` followed by `ONE`.
pub fn fields(field: usize, count: usize) -> Vec<u8> {
    let mut record = vec![b'a'; field];
    record.extend_from_slice(ONE);

    let mut data = Vec::with_capacity(record.len() * count);
    for _ in 0..count {
        data.extend_from_slice(&record);
    }
    data
}

/// Same, terminated by `TWO` instead.
pub fn lines(field: usize, count: usize) -> Vec<u8> {
    let mut record = vec![b'a'; field];
    record.extend_from_slice(TWO);

    let mut data = Vec::with_capacity(record.len() * count);
    for _ in 0..count {
        data.extend_from_slice(&record);
    }
    data
}

/// Deterministic filler that does not consist of records, for whole-stream reads.
pub fn cycled(total: usize) -> Vec<u8> {
    (0..=255u8).cycle().take(total).collect()
}

/// Total size of the fields in `data` split by `needle`, i.e. the loop every cell runs.
///
/// Callers wrap the result in `black_box`: this is the measured work, not the number it returns.
pub fn drain<'d, R: Read<'d>>(mut reader: R, needle: &[u8]) -> usize {
    let mut total = 0;
    while let Ok((field, true)) = reader.read_until(needle) {
        total += field.len();
    }
    total
}

/// A buffered reader with a chosen capacity: what `Take` and the scan-first paths sit behind.
pub fn std_reader(data: &[u8], capacity: usize) -> StdReader<&[u8]> {
    StdReader::new(BufReader::with_capacity(capacity, data))
}

/// Forwards everything to a slice but never overrides [`Read::peek_slice`], which is what a
/// third-party adapter looks like to `Take` and what its byte-at-a-time fallback is for.
pub struct Windowless<'d>(pub &'d [u8]);

impl<'d> Read<'d> for Windowless<'d> {
    fn peek_byte(&mut self) -> parsio::ReadResult<u8> {
        self.0.peek_byte()
    }

    fn advance(&mut self, n: usize) -> parsio::ReadResult<()> {
        self.0.advance(n)
    }

    fn read_byte(&mut self) -> parsio::ReadResult<u8> {
        self.0.read_byte()
    }

    fn read_slice(&mut self, len: usize) -> parsio::ReadResult<Cow<'d, [u8]>> {
        self.0.read_slice(len)
    }

    fn read_array<const N: usize>(&mut self) -> parsio::ReadResult<Cow<'d, [u8; N]>> {
        self.0.read_array()
    }

    fn read_till_eof(&mut self) -> parsio::ReadResult<Cow<'d, [u8]>> {
        self.0.read_till_eof()
    }

    fn read_until(&mut self, needle: &[u8]) -> parsio::ReadResult<(Cow<'d, [u8]>, bool)> {
        self.0.read_until(needle)
    }

    fn is_eof(&mut self) -> parsio::ReadResult<bool> {
        self.0.is_eof()
    }

    fn remaining(&self) -> Option<usize> {
        self.0.remaining()
    }
}
