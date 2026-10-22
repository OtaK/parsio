// The Linux-only half of `benches/instructions.rs`, included at its crate root (an `include!`d
// file cannot carry inner doc comments, and the harness has to sit at the crate root anyway).

use std::borrow::Cow;
use std::hint::black_box;

use gungraun::{Callgrind, EventKind, LibraryBenchmarkConfig, prelude::*};
use parsio::{DepthAwareReader, Read, Take, Write};

use crate::common::{ABSENT, ONE, TWO, Windowless};

/// Streams are built by the harness and passed in, so their cost is not attributed to the cell —
/// that is what keeps these counts bit-for-bit comparable across runs.
fn field_stream(field: usize, count: usize) -> Vec<u8> {
    crate::common::fields(field, count)
}

fn line_stream(field: usize, count: usize) -> Vec<u8> {
    crate::common::lines(field, count)
}

// --- read_until: every impl, same data, one- and two-byte needles ---------------------------

/// Reads one field at an entered depth, through the guard: the shape the guard exists for.
fn read_field_under_guard<'d, R: Read<'d>>(
    reader: &mut DepthAwareReader<R>,
) -> parsio::ReadResult<(Cow<'d, [u8]>, bool)> {
    let mut guard = reader.enter().expect("within the default limit");
    guard.read_until_match(ONE)
}

#[library_benchmark]
#[bench::field_20((field_stream(20, 4 << 10), ONE))]
#[bench::field_512((field_stream(512, 1 << 10), ONE))]
#[bench::crlf_20((line_stream(20, 4 << 10), TWO))]
fn slice_read_until((data, needle): (Vec<u8>, &'static [u8])) -> usize {
    black_box(crate::common::drain(
        black_box(&data[..]),
        black_box(needle),
    ))
}

#[library_benchmark]
#[bench::field_20((field_stream(20, 4 << 10), ONE))]
#[bench::field_512((field_stream(512, 1 << 10), ONE))]
#[bench::crlf_20((line_stream(20, 4 << 10), TWO))]
fn std_reader_read_until((data, needle): (Vec<u8>, &'static [u8])) -> usize {
    black_box(crate::common::drain(
        crate::common::std_reader(black_box(&data), 8 << 10),
        black_box(needle),
    ))
}

#[library_benchmark]
#[bench::field_20((field_stream(20, 4 << 10), ONE))]
#[bench::field_512((field_stream(512, 1 << 10), ONE))]
#[bench::crlf_20((line_stream(20, 4 << 10), TWO))]
fn vec_read_until((data, needle): (Vec<u8>, &'static [u8])) -> usize {
    black_box(crate::common::drain(black_box(data), black_box(needle)))
}

#[library_benchmark]
#[bench::field_20(field_stream(20, 4 << 10))]
fn depth_aware_read_until(data: Vec<u8>) -> usize {
    let mut reader = DepthAwareReader::from_reader(black_box(&data[..]));
    let mut total = 0;
    while let Ok((field, true)) = read_field_under_guard(&mut reader) {
        total += field.len();
    }
    black_box(total)
}

#[library_benchmark]
#[bench::absent(field_stream(20, 4 << 10))]
fn slice_read_until_absent(data: Vec<u8>) -> usize {
    black_box(crate::common::drain(
        black_box(&data[..]),
        black_box(ABSENT),
    ))
}

#[library_benchmark]
#[bench::field_20(field_stream(20, 4 << 10))]
fn cow_borrowed_read_until(data: Vec<u8>) -> usize {
    black_box(crate::common::drain(
        Cow::Borrowed(black_box(&data[..])),
        black_box(ONE),
    ))
}

#[library_benchmark]
#[bench::field_20(field_stream(20, 4 << 10))]
fn cow_owned_read_until(data: Vec<u8>) -> usize {
    black_box(crate::common::drain(
        Cow::Owned(black_box(data)),
        black_box(ONE),
    ))
}

// --- Take: the window scan, its clamp, and the byte-at-a-time fallback -----------------------

#[library_benchmark]
#[bench::field_20((field_stream(20, 4 << 10), ONE))]
#[bench::field_512((field_stream(512, 1 << 10), ONE))]
#[bench::crlf_20((line_stream(20, 4 << 10), TWO))]
#[bench::absent((field_stream(20, 4 << 10), ABSENT))]
fn take_slice_read_until((data, needle): (Vec<u8>, &'static [u8])) -> usize {
    let limit = data.len();
    black_box(crate::common::drain(
        Take::new(black_box(&data[..]), limit),
        black_box(needle),
    ))
}

#[library_benchmark]
#[bench::field_20((field_stream(20, 4 << 10), ONE))]
#[bench::field_512((field_stream(512, 1 << 10), ONE))]
#[bench::crlf_20((line_stream(20, 4 << 10), TWO))]
fn take_std_reader_read_until((data, needle): (Vec<u8>, &'static [u8])) -> usize {
    let limit = data.len();
    black_box(crate::common::drain(
        Take::new(crate::common::std_reader(black_box(&data), 8 << 10), limit),
        black_box(needle),
    ))
}

#[library_benchmark]
#[bench::field_20(field_stream(20, 4 << 10))]
fn take_vec_read_until(data: Vec<u8>) -> usize {
    let limit = data.len();
    black_box(crate::common::drain(
        Take::new(black_box(data), limit),
        black_box(ONE),
    ))
}

// The limit cuts mid-field, so the last field ends at the clamp rather than at a needle.
#[library_benchmark]
#[bench::half(field_stream(20, 4 << 10))]
fn take_read_until_cut_mid_field(data: Vec<u8>) -> usize {
    let limit = data.len() / 2;
    black_box(crate::common::drain(
        Take::new(black_box(&data[..]), limit),
        black_box(ONE),
    ))
}

// The needle lies beyond the limit: the whole window is one unmatched field.
#[library_benchmark]
#[bench::field_20(field_stream(20, 4 << 10))]
fn take_read_until_needle_beyond_limit(data: Vec<u8>) -> usize {
    // One field is appended past the limit, holding the needle: the scan drains the whole
    // window without matching. The needle is real data, so the path taken is "not found
    // before the limit", not "absent from the stream".
    let mut hidden = data;
    hidden.extend_from_slice(b"#");
    let limit = hidden.len() - b"#".len();
    black_box(crate::common::drain(
        Take::new(black_box(&hidden[..]), limit),
        black_box(b"#"),
    ))
}

// A reader with no window: the fallback path, and the only cell that runs it.
#[library_benchmark]
#[bench::field_20(field_stream(20, 4 << 10))]
#[bench::field_512(field_stream(512, 1 << 10))]
fn take_windowless_read_until(data: Vec<u8>) -> usize {
    let limit = data.len();
    black_box(crate::common::drain(
        Take::new(Windowless(black_box(&data[..])), limit),
        black_box(ONE),
    ))
}

// --- Tiny buffers: windows shorter than the needle, drained between reads --------------------

#[library_benchmark]
#[bench::cap_1((line_stream(20, 4 << 10), 1))]
#[bench::cap_3((line_stream(20, 4 << 10), 3))]
fn tiny_std_reader_read_until((data, capacity): (Vec<u8>, usize)) -> usize {
    black_box(crate::common::drain(
        crate::common::std_reader(black_box(&data), capacity),
        black_box(TWO),
    ))
}

#[library_benchmark]
#[bench::cap_1((line_stream(20, 4 << 10), 1))]
#[bench::cap_3((line_stream(20, 4 << 10), 3))]
fn tiny_take_std_reader_read_until((data, capacity): (Vec<u8>, usize)) -> usize {
    let limit = data.len();
    black_box(crate::common::drain(
        Take::new(crate::common::std_reader(black_box(&data), capacity), limit),
        black_box(TWO),
    ))
}

// --- Primitives: the per-byte baseline the scans have to beat --------------------------------

#[library_benchmark]
#[bench::read_byte(crate::common::cycled(1 << 16))]
fn slice_read_byte(data: Vec<u8>) -> u64 {
    let mut reader = black_box(&data[..]);
    let mut total = 0u64;
    while let Ok(byte) = reader.read_byte() {
        total += u64::from(black_box(byte));
    }
    black_box(total)
}

#[library_benchmark]
#[bench::read_slice_512(crate::common::cycled(1 << 16))]
fn slice_read_slice(data: Vec<u8>) -> usize {
    let mut reader = black_box(&data[..]);
    let mut total = 0;
    while let Ok(chunk) = reader.read_slice(512) {
        // A slice read is a pointer bump: without this the whole loop collapses into arithmetic.
        total += black_box(chunk.len());
    }
    black_box(total)
}

#[library_benchmark]
#[bench::read_till_eof(crate::common::cycled(1 << 16))]
fn take_read_till_eof(data: Vec<u8>) -> usize {
    let limit = data.len();
    let mut reader = Take::new(black_box(&data[..]), limit);
    black_box(reader.read_till_eof().expect("bounded read").len())
}

// --- Write side ------------------------------------------------------------------------------

#[library_benchmark]
#[bench::write_all(crate::common::cycled(512))]
fn vec_write_all(payload: Vec<u8>) -> usize {
    let mut sink = Vec::with_capacity(payload.len());
    sink.write_all(black_box(&payload)).expect("infallible");
    black_box(sink.len())
}

#[library_benchmark]
#[bench::write_all(crate::common::cycled(512))]
fn slice_write_all(payload: Vec<u8>) -> usize {
    let mut buffer = vec![0u8; payload.len()];
    let mut sink = &mut buffer[..];
    sink.write_all(black_box(&payload)).expect("fits");
    black_box(sink.len())
}

#[library_benchmark]
#[bench::write_all(crate::common::cycled(512))]
fn std_writer_write_all(payload: Vec<u8>) -> usize {
    let mut sink = parsio::StdWriter::from(Vec::with_capacity(payload.len()));
    sink.write_all(black_box(&payload)).expect("infallible");
    black_box(sink.into_inner().len())
}

#[library_benchmark]
#[bench::overflow(crate::common::cycled(513))]
fn slice_write_all_overflow(payload: Vec<u8>) -> bool {
    let mut buffer = vec![0u8; payload.len() - 1];
    let mut sink = &mut buffer[..];
    black_box(sink.write_all(black_box(&payload)).is_err())
}

#[library_benchmark]
#[bench::write_vectored(crate::common::cycled(512))]
fn vec_write_vectored(payload: Vec<u8>) -> usize {
    let (head, tail) = payload.split_at(256);
    let mut sink = Vec::with_capacity(payload.len());
    let written = sink
        .write_vectored(&[black_box(head), black_box(tail)])
        .expect("infallible");
    black_box(written)
}

library_benchmark_group!(
    name = read_until,
    benchmarks = [
        slice_read_until,
        std_reader_read_until,
        vec_read_until,
        depth_aware_read_until,
        slice_read_until_absent,
        cow_borrowed_read_until,
        cow_owned_read_until,
    ]
);

library_benchmark_group!(
    name = take,
    benchmarks = [
        take_slice_read_until,
        take_std_reader_read_until,
        take_vec_read_until,
        take_read_until_cut_mid_field,
        take_read_until_needle_beyond_limit,
        take_windowless_read_until,
    ]
);

library_benchmark_group!(
    name = tiny_windows,
    benchmarks = [tiny_std_reader_read_until, tiny_take_std_reader_read_until]
);

library_benchmark_group!(
    name = primitives,
    benchmarks = [slice_read_byte, slice_read_slice, take_read_till_eof]
);

library_benchmark_group!(
    name = write,
    benchmarks = [
        vec_write_all,
        slice_write_all,
        slice_write_all_overflow,
        std_writer_write_all,
        vec_write_vectored,
    ]
);

main!(
    config = LibraryBenchmarkConfig::default().tool(
        // Gated on `Ir` alone: Callgrind's cache simulation moves a few percent on counts in the
        // tens, depending on where the loader put things.
        //
        // 10% because `Ir` is bit-identical for a given binary but shifts up to ~6.5% when a
        // rebuild moves inlining around (measured across the cells here), and a soft limit that
        // trips on unrelated edits stops being believed. Real regressions in this suite are an
        // order of magnitude bigger: dropping `Take`'s window re-scan costs +215% to +4238%.
        //
        // Hard limits are absolute counts and are not used: they would need re-tuning on every
        // compiler bump.
        Callgrind::default().soft_limits([(EventKind::Ir, 10f64)])
    );
    library_benchmark_groups = read_until, take, tiny_windows, primitives, write
);
