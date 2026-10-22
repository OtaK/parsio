use std::borrow::Cow;
use std::io::BufReader;

use parsio::{Read, ReadError, StdReader, Take, Write, WriteError};

fn byte_stream() -> Vec<u8> {
    (0..=255u8).collect()
}

#[test]
fn std_advance_skips_exactly_n_across_refills() {
    let data = byte_stream();
    let mut r = StdReader::new(BufReader::with_capacity(16, &data[..]));

    assert_eq!(r.peek_byte().unwrap(), 0);
    r.advance(20).unwrap(); // spans two buffer refills
    assert_eq!(r.read_byte().unwrap(), 20);
    r.advance(1).unwrap();
    assert_eq!(r.read_byte().unwrap(), 22);
}

#[test]
fn std_advance_past_eof_errors() {
    let data = [9u8; 10];
    let mut r = StdReader::new(BufReader::with_capacity(16, &data[..]));

    assert!(matches!(
        r.advance(11),
        Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof))
    ));
}

/// Serves data, but fails the first refill with `Interrupted`, like a signal landing on a
/// pipe: the error is transient, data follows.
struct InterruptedOnce<'a> {
    data: &'a [u8],
    armed: bool,
}

impl std::io::Read for InterruptedOnce<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.armed {
            self.armed = true;
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        let n = buf.len().min(self.data.len());
        buf[..n].copy_from_slice(&self.data[..n]);
        self.data = &self.data[n..];
        Ok(n)
    }
}

/// Regression: `peek_byte`/`advance`/`read_slice`/`read_until` refill through `fill_buf`, which
/// does not retry `Interrupted` on its own, so one EINTR surfaced as a permanent parse error
/// while `read_byte`/`read_array`/`read_till_eof` (via std's `read_exact`/`read_to_end`) already
/// retried it.
#[test]
fn std_transient_interrupted_is_retried_not_surfaced() {
    let data = b"abc,def";
    let mk = || {
        StdReader::new(BufReader::with_capacity(
            2,
            InterruptedOnce {
                data: &data[..],
                armed: false,
            },
        ))
    };

    // peek_byte -> require_buffer_filled
    assert_eq!(mk().peek_byte().unwrap(), b'a');

    // advance -> refill loop, then read_byte (which must still work through the guard bytes)
    let mut r = mk();
    r.advance(3).unwrap();
    assert_eq!(r.read_byte().unwrap(), b',');

    // read_slice -> chunked path (capacity 2 < 4 forces the loop)
    assert_eq!(&*mk().read_slice(4).unwrap(), b"abc,");

    // read_until -> scan loop
    let (field, found) = mk().read_until_match(b",").unwrap();
    assert_eq!((field.as_ref(), found), (&b"abc"[..], true));
}

/// Counts `read()` calls; always at EOF
struct CountingEof(usize);

impl std::io::Read for CountingEof {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        self.0 += 1;
        Ok(0)
    }
}

/// Regression: the refill retry called `fill_buf` twice, so every EOF cost a second `read()`
/// (a second Ctrl-D on a TTY)
#[test]
fn std_eof_is_one_read() {
    let mut r = StdReader::new(BufReader::new(CountingEof(0)));
    assert!(r.is_eof().unwrap());
    assert_eq!(r.into_inner().into_inner().0, 1);

    let mut r = StdReader::new(BufReader::new(CountingEof(0)));
    assert!(r.peek_byte().is_err());
    assert_eq!(r.into_inner().into_inner().0, 1);
}

/// Regression: `Take::read_until` copied every field into a fresh `Vec`, losing zero-copy
#[test]
fn take_read_until_borrows_over_slices() {
    let data = b"ab,cd,ef";
    let mut take = Take::new(&data[..], data.len());
    for field in [&b"ab"[..], b"cd"] {
        let (got, found) = take.read_until_match(b",").unwrap();
        assert!(found);
        assert!(matches!(got, Cow::Borrowed(f) if f == field));
    }
}

/// Serves `data` then fails every read with `WouldBlock`, like a non-blocking socket
struct BlocksAfter<'a>(&'a [u8]);

impl std::io::Read for BlocksAfter<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.0.is_empty() {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        let n = buf.len().min(self.0.len());
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        Ok(n)
    }
}

/// Regression: the inner read ate 4 bytes then failed, but `Take` kept its whole budget, so a
/// retry read past the frame
#[test]
fn take_budget_drops_after_partial_inner_failure() {
    let reader = StdReader::new(BufReader::with_capacity(4, BlocksAfter(b"abcd")));
    let mut take = Take::new(reader, 8);
    assert!(matches!(
        take.read_slice(8),
        Err(ReadError::IoError(std::io::ErrorKind::WouldBlock))
    ));
    assert_eq!(take.remaining(), 0);
    assert!(take.read_byte().is_err());
}

/// Capacity 0 never buffers, so it would silently read as EOF
#[test]
#[should_panic(expected = "capacity > 0")]
fn std_reader_rejects_zero_capacity() {
    StdReader::new(BufReader::with_capacity(0, &b"abc"[..]));
}

/// Regression: with std's `Read`/`BufRead` in scope, `take` resolved to `io::Read::take` and
/// `read_until` was ambiguous; `StdReader` over a non-`BufRead` reader wasn't `BufRead` at all
#[test]
fn parsio_methods_resolve_next_to_std_io_traits() {
    use parsio::LimitedReadExt;
    #[allow(unused_imports)]
    use std::io::{BufRead, Read as _};

    let mut slice: &[u8] = b"ab,cd\nrest";
    let mut take = slice.limited(4);
    assert_eq!(&*take.read_until_match(b",").unwrap().0, b"ab");

    let mut r = StdReader::new(BufReader::new(BlocksAfter(b"ab,cd\nrest")));
    assert_eq!(&*r.read_until_match(b",").unwrap().0, b"ab");
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    assert_eq!(line, "cd\n");
}

#[test]
fn std_read_slice_bounds_allocation() {
    let data = [0u8; 4];
    let mut r = StdReader::new(BufReader::new(&data[..]));

    assert!(matches!(
        r.read_slice(usize::MAX / 2),
        Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof))
    ));
}

#[test]
fn std_read_slice_exact_and_short() {
    let data = [1u8, 2, 3, 4];

    let mut r = StdReader::new(BufReader::with_capacity(2, &data[..]));
    assert_eq!(&*r.read_slice(4).unwrap(), &[1, 2, 3, 4]);

    let mut r = StdReader::new(BufReader::with_capacity(2, &data[..]));
    assert!(r.read_slice(6).is_err());
}

#[test]
fn slice_write_all_leaves_buffer_untouched_on_write_zero() {
    let mut buf = [0u8; 2];

    let remaining = {
        let mut w: &mut [u8] = &mut buf;
        assert!(matches!(
            w.write_all(&[1, 2, 3]),
            Err(WriteError::IoError(std::io::ErrorKind::WriteZero))
        ));
        w.len()
    };

    assert_eq!(remaining, 2, "buffer must not have been consumed");
    assert_eq!(buf, [0, 0]);

    {
        let mut w: &mut [u8] = &mut buf;
        w.write_all(&[1, 2]).unwrap();
        assert!(w.is_empty());
    }
    assert_eq!(buf, [1, 2]);
}

#[test]
fn slice_write_partial_and_vectored() {
    let mut buf = [0u8; 4];

    {
        let mut w: &mut [u8] = &mut buf;
        assert_eq!(w.write(&[1, 2, 3]).unwrap(), 3);
        assert_eq!(w.write_vectored(&[&[4, 5], &[6]]).unwrap(), 1);
        assert_eq!(w.write_vectored(&[&[9]]).unwrap(), 0);
    }

    assert_eq!(buf, [1, 2, 3, 4]);
}

#[test]
fn slice_reader_roundtrip() {
    let data = [1u8, 2, 3, 4];
    let mut r: &[u8] = &data;

    let head: Cow<[u8; 2]> = r.read_array().unwrap();
    assert_eq!(&*head, &[1, 2]);
    assert_eq!(&*r.read_slice(1).unwrap(), &[3]);
    assert_eq!(r.read_byte().unwrap(), 4);
    assert!(r.read_till_eof().unwrap().is_empty());
    assert!(r.read_byte().is_err());
}

#[test]
fn vec_reader_roundtrip() {
    let mut r: Vec<u8> = vec![1, 2, 3, 4];

    assert_eq!(r.peek_byte().unwrap(), 1);
    assert_eq!(&*r.read_slice(2).unwrap(), &[1, 2]);
    assert_eq!(r.read_byte().unwrap(), 3);
    assert_eq!(&*r.read_till_eof().unwrap(), &[4]);
    assert!(r.is_empty());
}

#[test]
fn std_writer_roundtrip() {
    let mut w = parsio::StdWriter::new(Vec::new());
    w.write_all(b"hello").unwrap();
    assert_eq!(Write::write_vectored(&mut w, &[b" ", b"world"]).unwrap(), 6);
    w.flush().unwrap();
    assert_eq!(w.into_inner(), b"hello world");
}

/// Regression: `Take::advance` used to call itself, so any legal `advance` blew the stack.
#[test]
fn take_advance_delegates_instead_of_recursing() {
    let mut take = Take::new(&b"abcdef"[..], 4);

    take.advance(2).unwrap();
    assert_eq!(take.remaining(), 2);
    assert_eq!(take.read_byte().unwrap(), b'c');
    assert!(
        take.advance(2).is_err(),
        "advancing past the limit must fail"
    );
    assert_eq!(
        take.remaining(),
        1,
        "the failed advance must not consume the budget"
    );
}

/// Regression: `Take::peek_byte` ignored the limit, so the adapter handed out bytes it had no
/// right to read.
#[test]
fn take_peek_respects_the_limit() {
    let mut take = Take::new(&b"abc"[..], 2);

    assert_eq!(take.peek_byte().unwrap(), b'a');
    assert_eq!(take.read_byte().unwrap(), b'a');
    assert_eq!(take.peek_byte().unwrap(), b'b');
    assert_eq!(take.read_byte().unwrap(), b'b');
    assert!(take.is_exhausted());
    assert!(
        take.peek_byte().is_err(),
        "peeking at the limit must fail, b'c' is out of bounds"
    );
    assert!(take.read_byte().is_err());
}

/// The limit also bounds `read_until`, which `Take` implements itself.
#[test]
fn take_read_until_stops_at_the_limit() {
    let mut take = Take::new(&b"a,b,c"[..], 3);

    let (field, found) = take.read_until_match(b",").unwrap();
    assert_eq!((field.as_ref(), found), (&b"a"[..], true));

    let (rest, found) = take.read_until_match(b",").unwrap();
    assert_eq!(
        (rest.as_ref(), found),
        (&b"b"[..], false),
        "the limit hides the second comma"
    );
}

/// Regression for the shape `Take` cannot forward: the inner reader's scan is unbounded, so an
/// adapter that forwarded would consume past its limit and report delimiters outside it.
#[test]
fn take_read_until_never_reaches_past_the_limit() {
    // The limit stops before the second delimiter: the trailing read must not pick it up.
    let mut take = Take::new(&b"ab,cd,ef"[..], 5);
    let (field, found) = take.read_until_match(b",").unwrap();
    assert_eq!((field.as_ref(), found), (&b"ab"[..], true));
    assert_eq!(
        take.read_till_eof().unwrap().as_ref(),
        b"cd",
        "the limit is 5 bytes"
    );

    // A delimiter past the limit must not be reported as found.
    let mut take = Take::new(&b"abc,def"[..], 3);
    let (field, found) = take.read_until_match(b",").unwrap();
    assert_eq!(
        (field.as_ref(), found),
        (&b"abc"[..], false),
        "',' lies beyond the limit"
    );
    assert!(take.read_till_eof().unwrap().is_empty());
}

/// `Take` reads no further than its limit, even when the inner stream has more.
#[test]
fn take_read_slice_and_eof_are_bounded() {
    let mut take = Take::new(&b"abcdef"[..], 3);

    assert_eq!(take.read_slice(3).unwrap().as_ref(), b"abc");
    assert!(take.read_slice(1).is_err());
    assert!(take.read_till_eof().unwrap().is_empty());
}
