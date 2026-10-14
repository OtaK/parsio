//! `read_until` across every impl: parity, borrowedness, and the multi-byte needle edges.

use std::borrow::Cow;
use std::io::BufReader;

use parsio::{DepthAwareReader, Read, StdReader, Take};

/// Drains a reader with `read_until`, stopping after the EOF report.
fn collect<'d, R: Read<'d>>(mut reader: R, needle: &[u8]) -> Vec<(Vec<u8>, bool)> {
    let mut out = Vec::new();
    loop {
        let (field, found) = reader.read_until(needle).unwrap();
        out.push((field.to_vec(), found));
        if !found {
            return out;
        }
    }
}

const DATA: &[u8] = b"alpha,beta,,gamma\r\nno-delimiter-at-all,,tail";

fn needles() -> Vec<Vec<u8>> {
    vec![
        b",".to_vec(),
        b"\r\n".to_vec(),
        b"beta,,".to_vec(),
        b"a".to_vec(),
        b"zz".to_vec(),
        vec![0xff, 0xfe],
        DATA.to_vec(), // the whole input as a needle
        {
            let mut long = DATA.to_vec();
            long.push(b'x'); // longer than the input
            long
        },
    ]
}

#[test]
fn every_impl_agrees() {
    for needle in needles() {
        let expect = collect(DATA, &needle);

        assert_eq!(collect(DATA.to_vec(), &needle), expect, "Vec<u8>");
        assert_eq!(
            collect(Cow::Borrowed(DATA), &needle),
            expect,
            "Cow::Borrowed"
        );
        assert_eq!(
            collect(Cow::Owned(DATA.to_vec()), &needle),
            expect,
            "Cow::Owned"
        );
        assert_eq!(
            collect(DepthAwareReader::from_reader(DATA), &needle),
            expect,
            "DepthAwareReader<&[u8]>"
        );
        assert_eq!(collect(Box::new(DATA), &needle), expect, "Box<&[u8]>");
        assert_eq!(
            collect(Take::new(DATA, DATA.len()), &needle),
            expect,
            "Take<&[u8]> (limit covers the input)"
        );
        assert_eq!(
            collect(Take::new(DATA.to_vec(), DATA.len()), &needle),
            expect,
            "Take<Vec<u8>>"
        );
        assert_eq!(
            collect(Take::new(Cow::Borrowed(DATA), DATA.len()), &needle),
            expect,
            "Take<Cow<[u8]>>"
        );

        // A limit that cuts the stream short: `read_until` must not look past it, so the same
        // needle that matched above may now report "not found".
        let limit = DATA.len() - 5;
        let window = &DATA[..limit];
        let expect_bounded = collect(window, &needle);
        assert_eq!(
            collect(Take::new(DATA, limit), &needle),
            expect_bounded,
            "Take<&[u8]> (limit {limit})"
        );
        assert_eq!(
            collect(Take::new(DATA.to_vec(), limit), &needle),
            expect_bounded,
            "Take<Vec<u8>> (limit {limit})"
        );
        assert_eq!(
            collect(Take::new(Cow::Borrowed(DATA), limit), &needle),
            expect_bounded,
            "Take<Cow<[u8]>> (limit {limit})"
        );
        assert_eq!(
            collect(
                Take::new(StdReader::new(BufReader::new(DATA)), limit),
                &needle
            ),
            expect_bounded,
            "Take<StdReader> (limit {limit})"
        );

        // The tiny buffers that force needles across refills, with the limit on top.
        for capacity in [1, 3] {
            assert_eq!(
                collect(
                    Take::new(
                        StdReader::new(BufReader::with_capacity(capacity, DATA)),
                        limit
                    ),
                    &needle
                ),
                expect_bounded,
                "Take<StdReader<cap={capacity}>> (limit {limit})"
            );
            assert_eq!(
                collect(
                    DepthAwareReader::from_reader(Take::new(
                        StdReader::new(BufReader::with_capacity(capacity, DATA)),
                        limit
                    )),
                    &needle
                ),
                expect_bounded,
                "DepthAwareReader<Take<StdReader<cap={capacity}>>> (limit {limit})"
            );
        }

        // A one-byte buffer forces every needle to straddle chunk boundaries.
        for capacity in [1, 3, 64] {
            assert_eq!(
                collect(
                    StdReader::new(BufReader::with_capacity(capacity, DATA)),
                    &needle
                ),
                expect,
                "StdReader<cap={capacity}>"
            );
            assert_eq!(
                collect(
                    DepthAwareReader::from_reader(StdReader::new(BufReader::with_capacity(
                        capacity, DATA
                    ))),
                    &needle
                ),
                expect,
                "DepthAwareReader<StdReader<cap={capacity}>>"
            );
        }

        let mut inner: &[u8] = DATA;
        assert_eq!(collect(&mut inner, &needle), expect, "&mut &[u8]");
    }
}

#[test]
fn borrowedness_follows_the_impl() {
    let data = b"head,tail";

    let mut slice: &[u8] = &data[..];
    let (field, found) = slice.read_until(b",").unwrap();
    assert!(matches!(field, Cow::Borrowed(_)), "&[u8] must borrow");
    assert!(found);

    let mut cow: Cow<[u8]> = Cow::Borrowed(&data[..]);
    let (field, _) = cow.read_until(b",").unwrap();
    assert!(
        matches!(field, Cow::Borrowed(_)),
        "Cow::Borrowed must borrow"
    );

    let mut vec: Vec<u8> = data.to_vec();
    let (field, _) = vec.read_until(b",").unwrap();
    assert!(matches!(field, Cow::Owned(_)), "Vec<u8> cannot borrow");

    let mut reader = StdReader::new(BufReader::new(&data[..]));
    let (field, _) = reader.read_until(b",").unwrap();
    assert!(matches!(field, Cow::Owned(_)), "StdReader cannot borrow");

    let mut depth = DepthAwareReader::from_reader(&data[..]);
    let (field, _) = depth.read_until(b",").unwrap();
    assert!(
        matches!(field, Cow::Borrowed(_)),
        "DepthAwareReader must forward"
    );
}

#[test]
fn delimiter_is_consumed_and_cursor_lands_after_it() {
    let data = b"field,rest";
    let mut reader: &[u8] = &data[..];

    let (field, found) = reader.read_until(b",").unwrap();
    assert_eq!((field.as_ref(), found), (&b"field"[..], true));
    assert_eq!(
        reader, b"rest",
        "the delimiter and everything before it is consumed"
    );

    let (tail, found) = reader.read_until(b"!").unwrap();
    assert_eq!(
        (tail.as_ref(), found),
        (&b"rest"[..], false),
        "no match: rest, nothing left"
    );
    assert!(reader.is_empty());
}

#[test]
fn empty_needle_is_invalid_input() {
    fn expect_invalid<T: std::fmt::Debug>(name: &str, got: parsio::ReadResult<T>) {
        match got {
            Err(parsio::ReadError::IoError(std::io::ErrorKind::InvalidInput)) => {}
            other => panic!("{name}: expected InvalidInput, got {other:?}"),
        }
    }

    let mut slice: &[u8] = DATA;
    expect_invalid("&[u8]", slice.read_until(b""));

    let mut vec: Vec<u8> = DATA.to_vec();
    expect_invalid("Vec<u8>", vec.read_until(b""));

    let mut cow: Cow<[u8]> = Cow::Borrowed(DATA);
    expect_invalid("Cow::Borrowed", cow.read_until(b""));

    let mut cow: Cow<[u8]> = Cow::Owned(DATA.to_vec());
    expect_invalid("Cow::Owned", cow.read_until(b""));

    let mut std_reader = StdReader::new(BufReader::new(DATA));
    expect_invalid("StdReader", std_reader.read_until(b""));

    let mut depth = DepthAwareReader::from_reader(DATA);
    expect_invalid("DepthAwareReader", depth.read_until(b""));

    let mut take = Take::new(DATA, DATA.len());
    expect_invalid("Take", take.read_until(b""));

    let mut boxed: Box<&[u8]> = Box::new(DATA);
    expect_invalid("Box<&[u8]>", boxed.read_until(b""));

    // Nothing was consumed, so the readers are still usable.
    assert_eq!(slice.len(), DATA.len());
    assert_eq!(vec, DATA.to_vec());
}

#[test]
fn needle_at_boundaries() {
    // Match at offset 0.
    let mut r: &[u8] = b",tail";
    assert_eq!(r.read_until(b",").unwrap().0.as_ref(), b"");

    // Match at the very end, multi-byte.
    let mut r: &[u8] = b"head\r\n";
    let (head, found) = r.read_until(b"\r\n").unwrap();
    assert_eq!(
        (head.as_ref(), found, r.is_empty()),
        (&b"head"[..], true, true)
    );

    // Consecutive delimiters yield empty fields.
    let mut r: &[u8] = b",,";
    assert_eq!(r.read_until(b",").unwrap().0.as_ref(), b"");
    assert_eq!(r.read_until(b",").unwrap().0.as_ref(), b"");
    assert!(!r.read_until(b",").unwrap().1);

    // Empty input.
    let mut r: &[u8] = b"";
    let (rest, found) = r.read_until(b"x").unwrap();
    assert_eq!((rest.as_ref(), found), (&b""[..], false));
}

/// The overlap window must find a needle that only exists across two chunks.
#[test]
fn needle_straddling_chunks() {
    let data = b"aaabbbb";

    for capacity in [1, 2, 3, 4] {
        let mut reader = StdReader::new(BufReader::with_capacity(capacity, &data[..]));
        let (head, found) = reader.read_until(b"abbb").unwrap();
        assert_eq!(
            (head.as_ref(), found),
            (&b"aa"[..], true),
            "capacity {capacity}"
        );
        assert_eq!(reader.read_till_eof().unwrap().as_ref(), b"b");
    }
}

#[test]
fn long_needle_absent_from_short_stream() {
    let mut reader = StdReader::new(BufReader::with_capacity(2, &b"abc"[..]));
    let (rest, found) = reader.read_until(b"abcdefgh").unwrap();
    assert_eq!((rest.as_ref(), found), (&b"abc"[..], false));
}

/// A reader that forwards everything but never exposes a window ([`Read::peek_slice`] keeps its
/// default), which is what a third-party adapter looks like to `Take`.
struct Windowless<'d>(&'d [u8]);

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

/// A needle split across a refill boundary: the window that showed its first half was drained
/// before the second half arrived, so only checking the seam where it left off can catch the match.
#[test]
fn take_finds_needle_split_across_a_refill() {
    let data: Vec<u8> = (0..=255u8).collect();

    for capacity in 1..=4 {
        let mut take = Take::new(
            StdReader::new(BufReader::with_capacity(capacity, &data[..])),
            data.len(),
        );

        // `0x81, 0x82` sits at offset 0x81, i.e. one byte after a chunk boundary at this capacity
        // often enough for the seam to matter.
        let (field, found) = take.read_until(&[0x81, 0x82]).unwrap();
        assert_eq!((field.len(), found), (0x81, true), "capacity {capacity}");
        assert_eq!(
            take.read_till_eof().unwrap().len(),
            data.len() - 0x81 - 2,
            "capacity {capacity}: the rest of the stream"
        );
    }
}

/// A needle longer than any window the reader can show: no window ever holds it whole, so the
/// match can only be spotted as it completes.
#[test]
fn take_finds_needle_longer_than_the_window() {
    let data = b"xxabcde,rest";

    for capacity in 1..=4 {
        let mut take = Take::new(
            StdReader::new(BufReader::with_capacity(capacity, &data[..])),
            data.len(),
        );

        let (field, found) = take.read_until(b"abcde").unwrap();
        assert_eq!(
            (field.as_ref(), found),
            (&b"xx"[..], true),
            "capacity {capacity}"
        );
        assert_eq!(
            take.read_till_eof().unwrap().as_ref(),
            b",rest",
            "capacity {capacity}"
        );
    }
}

/// `Take` over a reader with no window has to walk it a byte at a time, and must still agree with
/// the impls that do show one — including where the limit hides the needle.
#[test]
fn take_over_a_reader_without_a_window() {
    let data: Vec<u8> = (0..=255u8).collect();
    assert!(
        Windowless(&data).peek_slice().is_empty(),
        "the default window is empty"
    );

    for needle in [&[0x81u8, 0x82][..], &[0xff, 0x00][..], b"absent"] {
        let expect = collect(&data[..], needle);
        assert_eq!(
            collect(Take::new(Windowless(&data), data.len()), needle),
            expect,
            "Take<Windowless>: needle {needle:?}"
        );
    }

    let mut take = Take::new(Windowless(&data), data.len());
    let (field, found) = take.read_until(&[0x81, 0x82]).unwrap();
    assert_eq!((field.len(), found), (0x81, true));
    assert_eq!(take.read_till_eof().unwrap().len(), data.len() - 0x81 - 2);

    // The limit applies to the byte-at-a-time path too.
    let mut take = Take::new(Windowless(b"ab,cd,ef"), 5);
    let (field, found) = take.read_until(b",").unwrap();
    assert_eq!((field.as_ref(), found), (&b"ab"[..], true));
    assert_eq!(take.read_till_eof().unwrap().as_ref(), b"cd");

    let mut take = Take::new(Windowless(b"abc,def"), 3);
    let (field, found) = take.read_until(b",").unwrap();
    assert_eq!(
        (field.as_ref(), found),
        (&b"abc"[..], false),
        "the limit hides the comma"
    );
    assert!(take.read_till_eof().unwrap().is_empty());
}
