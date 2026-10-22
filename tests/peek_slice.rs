//! `Read::peek_slice`: a partial, non-consuming view of what the reader can hand out.
//!
//! The contract has two halves, and `Take::read_until` leans on both: looking costs nothing and
//! advances nothing, and every byte the window shows is consumable as advertised. A window may
//! also be empty while the stream is not — that is documented looseness, not EOF.

use std::borrow::Cow;
use std::io::BufReader;

use parsio::{DepthAwareReader, Read, StdReader, Take};

/// Checks the properties every impl owes, then reads `data` back to make sure the window did not
/// disturb the stream.
fn check<'d, R: Read<'d>>(name: &str, mut reader: R, data: &'d [u8]) {
    let window = reader.peek_slice().to_vec();
    assert_eq!(
        reader.peek_slice(),
        window.as_slice(),
        "{name}: peek_slice must not advance, nor need two reads to settle"
    );

    if !window.is_empty() {
        let head = reader
            .read_slice(window.len())
            .unwrap_or_else(|e| panic!("{name}: window is not readable: {e:?}"));
        assert_eq!(
            head.as_ref(),
            window.as_slice(),
            "{name}: the window is not what read_slice hands out"
        );
    }

    let rest = reader
        .read_till_eof()
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    assert_eq!(
        rest.as_ref(),
        &data[window.len()..],
        "{name}: the stream after the window"
    );
}

#[test]
fn window_is_consumable_and_non_consuming() {
    let data: Vec<u8> = (0..=255u8).collect();

    check("&[u8]", &data[..], &data);
    check("Vec<u8>", data.clone(), &data);
    check("Cow::Borrowed", Cow::Borrowed(&data[..]), &data);
    check("Cow::Owned", Cow::Owned(data.clone()), &data);
    check(
        "StdReader",
        StdReader::new(BufReader::new(&data[..])),
        &data,
    );
    check(
        "DepthAwareReader<&[u8]>",
        DepthAwareReader::from_reader(&data[..]),
        &data,
    );
    check("Box<&[u8]>", Box::new(&data[..]), &data);
    check("Take<&[u8]>", Take::new(&data[..], data.len()), &data);

    let mut inner: &[u8] = &data;
    check("&mut &[u8]", &mut inner, &data);
}

/// An in-memory reader hides nothing, and the limit of an adapter is visible in the window.
#[test]
fn in_memory_readers_show_the_remaining_data() {
    let data = b"window";

    let mut slice: &[u8] = data;
    assert_eq!(slice.peek_slice(), data);
    assert_eq!(slice, data, "looking must not consume");

    slice.advance(3).unwrap();
    assert_eq!(slice.peek_slice(), b"dow");

    assert_eq!(data.to_vec().peek_slice(), data);
    assert_eq!(Cow::<[u8]>::Borrowed(&data[..]).peek_slice(), data);
    assert_eq!(Cow::<[u8]>::Owned(data.to_vec()).peek_slice(), data);
}

/// `StdReader` shows what its `BufReader` already holds: looking performs no I/O, and a drained
/// window refills on the next read rather than reporting EOF.
#[test]
fn std_reader_shows_what_is_buffered() {
    let data: Vec<u8> = (0..=255u8).collect();
    let mut reader = StdReader::new(BufReader::with_capacity(4, &data[..]));

    assert!(
        reader.peek_slice().is_empty(),
        "peeking must not read ahead"
    );
    assert_eq!(
        reader.peek_byte().unwrap(),
        0,
        "reading a byte does fill it"
    );
    assert_eq!(reader.peek_slice(), &data[..4]);

    reader.advance(2).unwrap();
    assert_eq!(reader.peek_slice(), &data[2..4]);

    reader.advance(2).unwrap();
    assert!(reader.peek_slice().is_empty(), "drained, but not EOF");
    assert_eq!(reader.peek_byte().unwrap(), 4);
}

/// `Take` clamps the window: the bytes past its limit are never shown, and the limit does not have
/// to fit in the stream.
#[test]
fn take_clamps_the_window_to_its_limit() {
    let data: Vec<u8> = (0..=255u8).collect();

    let mut take = Take::new(&data[..], 3);
    assert_eq!(take.peek_slice(), &data[..3]);
    take.advance(2).unwrap();
    assert_eq!(take.peek_slice(), &data[2..3]);
    take.advance(1).unwrap();
    assert!(take.peek_slice().is_empty(), "exhausted");
    assert!(take.peek_byte().is_err());

    let take = Take::new(&data[..], data.len() + 10);
    assert_eq!(take.peek_slice(), &data[..], "clamped by the stream");

    // Behind a `StdReader` nothing is buffered until something is read, and the limit is what
    // clamps the window then — not the 8-byte buffer.
    let mut take = Take::new(StdReader::new(BufReader::with_capacity(8, &data[..])), 4);
    assert!(take.peek_slice().is_empty(), "nothing buffered yet");
    assert_eq!(take.peek_byte().unwrap(), 0);
    assert_eq!(take.peek_slice(), &data[..4]);

    // The window goes through `DepthAwareReader` untouched.
    let depth = DepthAwareReader::from_reader(Take::new(&data[..], 2));
    assert_eq!(depth.peek_slice(), &data[..2]);
}
