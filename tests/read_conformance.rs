use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{BufReader, ErrorKind};

use parsio::{DepthAwareReader, Read, ReadError, ReadResult, StdReader, Take};

fn assert_slice(name: &str, op: &str, got: ReadResult<Cow<'_, [u8]>>, want: &[u8]) {
    match got {
        Ok(bytes) => assert_eq!(&bytes[..], want, "{name}: {op}"),
        Err(e) => panic!("{name}: {op}: expected Ok({want:?}), got Err({e:?})"),
    }
}

fn assert_array<const N: usize>(
    name: &str,
    op: &str,
    got: ReadResult<Cow<'_, [u8; N]>>,
    want: &[u8],
) {
    match got {
        Ok(bytes) => assert_eq!(&bytes[..], want, "{name}: {op}"),
        Err(e) => panic!("{name}: {op}: expected Ok({want:?}), got Err({e:?})"),
    }
}

fn assert_byte(name: &str, op: &str, got: ReadResult<u8>, want: u8) {
    match got {
        Ok(byte) => assert_eq!(byte, want, "{name}: {op}"),
        Err(e) => panic!("{name}: {op}: expected Ok({want}), got Err({e:?})"),
    }
}

fn assert_eof<T: std::fmt::Debug>(name: &str, op: &str, got: ReadResult<T>) {
    match got {
        Err(ReadError::IoError(ErrorKind::UnexpectedEof)) => {}
        other => panic!("{name}: {op}: expected IoError(UnexpectedEof), got {other:?}"),
    }
}

/// Happy paths, being a good citizen and reading streams till EOF
fn happy<'d, R: Read<'d>>(name: &str, mut reader: R, data: &[u8]) {
    let len = data.len();

    assert_byte(name, "peek_byte(0)", reader.peek_byte(), data[0]);
    assert_byte(
        name,
        "peek_byte(1) [idempotent]",
        reader.peek_byte(),
        data[0],
    );

    reader
        .advance(1)
        .unwrap_or_else(|e| panic!("{name}: advance(1): {e:?}"));
    assert_byte(name, "peek_byte after advance", reader.peek_byte(), data[1]);
    assert_byte(name, "read_byte", reader.read_byte(), data[1]);

    assert_slice(name, "read_slice(2)", reader.read_slice(2), &data[2..4]);
    assert_array(
        name,
        "read_array::<2>",
        reader.read_array::<2>(),
        &data[4..6],
    );
    assert_slice(name, "read_slice(0)", reader.read_slice(0), &[]);
    assert_array(name, "read_array::<0>", reader.read_array::<0>(), &[]);
    reader
        .advance(0)
        .unwrap_or_else(|e| panic!("{name}: advance(0): {e:?}"));

    // Drain the rest of the stream, ending exactly on empty.
    assert_byte(
        name,
        "read_byte (after zero-length ops)",
        reader.read_byte(),
        data[6],
    );
    reader
        .advance(len - 8)
        .unwrap_or_else(|e| panic!("{name}: advance(len - 8): {e:?}"));
    assert_byte(
        name,
        "read_byte (last byte)",
        reader.read_byte(),
        data[len - 1],
    );
    assert_slice(name, "read_till_eof (empty)", reader.read_till_eof(), &[]);
}

/// EOF/oversized reads/boundary checks
fn boundaries<'d, R: Read<'d>>(name: &str, mk: &impl Fn(&'d [u8]) -> R, data: &'d [u8]) {
    let len = data.len();

    macro_rules! exhausted {
        () => {{
            let mut reader = mk(data);
            reader
                .advance(len)
                .unwrap_or_else(|e| panic!("{name}: advance(len) to reach EOF: {e:?}"));
            reader
        }};
    }

    assert_eof(name, "peek_byte at EOF", exhausted!().peek_byte());
    assert_eof(name, "read_byte at EOF", exhausted!().read_byte());
    assert_eof(name, "advance(1) at EOF", exhausted!().advance(1));
    assert_eof(name, "read_slice(1) at EOF", exhausted!().read_slice(1));
    assert_eof(
        name,
        "read_array::<1> at EOF",
        exhausted!().read_array::<1>(),
    );
    assert_slice(
        name,
        "read_slice(0) at EOF",
        exhausted!().read_slice(0),
        &[],
    );
    assert_array(
        name,
        "read_array::<0> at EOF",
        exhausted!().read_array::<0>(),
        &[],
    );
    assert_slice(
        name,
        "read_till_eof at EOF",
        exhausted!().read_till_eof(),
        &[],
    );

    if len > 0 {
        assert_slice(
            name,
            "read_slice(len) (exact)",
            mk(data).read_slice(len),
            data,
        );
    }
    if len >= 4 {
        let head = &data[..4];
        assert_array(
            name,
            "read_array::<4> (exact)",
            mk(head).read_array::<4>(),
            head,
        );
    }

    assert_eof(name, "advance(len + 1)", mk(data).advance(len + 1));
    assert_eof(name, "read_slice(len + 1)", mk(data).read_slice(len + 1));
    assert_eof(name, "read_array::<257>", mk(data).read_array::<257>());
    // Must not try to allocate `len` bytes up front.
    assert_eof(
        name,
        "read_slice(usize::MAX / 2)",
        mk(data).read_slice(usize::MAX / 2),
    );
}

/// `boundaries` against a full stream and against an empty one: the empty case covers the
/// readers before they ever consumed a byte, which is a different state than being at EOF after
/// reading.
fn boundaries_all<'d, R: Read<'d>>(name: &str, mk: &impl Fn(&'d [u8]) -> R, data: &'d [u8]) {
    boundaries(name, mk, data);
    boundaries(name, mk, &[]);
}

/// `read_until` cells: single-byte and multi-byte needles that are present, plus one that is not.
/// Runs against the full stream and against an empty one.
fn until_all<'d, R: Read<'d>>(name: &str, mk: &impl Fn(&'d [u8]) -> R, data: &'d [u8]) {
    for needle in [&[0x80u8][..], &[0x81, 0x82][..], &[0xff, 0x00][..]] {
        let (expect, found) = match crate::find_model(data, needle) {
            Some(index) => (data[..index].to_vec(), true),
            None => (data.to_vec(), false),
        };

        let mut reader = mk(data);
        let (got, got_found) = match reader.read_until_match(needle) {
            Ok(value) => value,
            Err(e) => panic!("{name}: read_until({needle:?}): {e:?}"),
        };
        assert_eq!(
            (got.as_ref(), got_found),
            (expect.as_slice(), found),
            "{name}: read_until({needle:?})"
        );

        // The needle and everything before it is gone; the rest still reads.
        let rest = data[expect.len() + if found { needle.len() } else { 0 }..].to_vec();
        assert_eq!(
            reader.read_till_eof().unwrap().as_ref(),
            rest.as_slice(),
            "{name}: remaining after read_until({needle:?})"
        );
    }
}

/// Reference substring search for the cells above.
fn find_model(data: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() == 1 {
        data.iter().position(|byte| *byte == needle[0])
    } else {
        data.windows(needle.len())
            .position(|window| window == needle)
    }
}

fn data() -> Vec<u8> {
    (0..=255u8).collect()
}

#[test]
fn conformance_slice() {
    let data = data();
    happy("&[u8]", &data[..], &data);
    boundaries_all("&[u8]", &|d: &[u8]| d, &data);
    until_all("&[u8]", &|d: &[u8]| d, &data);
}

#[test]
fn conformance_vec() {
    let data = data();
    happy("Vec<u8>", data.clone(), &data);
    boundaries_all("Vec<u8>", &|d: &[u8]| d.to_vec(), &data);
    until_all("Vec<u8>", &|d: &[u8]| d.to_vec(), &data);
}

#[test]
fn conformance_vec_deque() {
    let data = data();
    // Back half first, front half pushed in front: the ring buffer wraps
    let wrapped = |d: &[u8]| {
        let mid = d.len() / 2;
        let mut q = VecDeque::with_capacity(d.len());
        q.extend(&d[mid..]);
        d[..mid].iter().rev().for_each(|&b| q.push_front(b));
        q
    };
    happy("VecDeque<u8>", VecDeque::from(data.clone()), &data);
    happy("VecDeque<u8> (wrapped)", wrapped(&data), &data);
    boundaries_all(
        "VecDeque<u8>",
        &|d: &[u8]| VecDeque::from(d.to_vec()),
        &data,
    );
    boundaries_all("VecDeque<u8> (wrapped)", &wrapped, &data);
    until_all(
        "VecDeque<u8>",
        &|d: &[u8]| VecDeque::from(d.to_vec()),
        &data,
    );
    until_all("VecDeque<u8> (wrapped)", &wrapped, &data);
}

#[test]
fn conformance_cow_borrowed() {
    let data = data();
    happy("Cow::Borrowed", Cow::Borrowed(&data[..]), &data);
    boundaries_all("Cow::Borrowed", &|d: &[u8]| Cow::Borrowed(d), &data);
    until_all("Cow::Borrowed", &|d: &[u8]| Cow::Borrowed(d), &data);
}

#[test]
fn conformance_cow_owned() {
    let data = data();
    happy("Cow::Owned", Cow::Owned(data.clone()), &data);
    boundaries_all("Cow::Owned", &|d: &[u8]| Cow::Owned(d.to_vec()), &data);
    until_all("Cow::Owned", &|d: &[u8]| Cow::Owned(d.to_vec()), &data);
}

#[test]
fn conformance_std_reader() {
    let data = data();
    for capacity in [1, 3, 8192] {
        let name = format!("StdReader<cap={capacity}>");
        happy(
            &name,
            StdReader::new(BufReader::with_capacity(capacity, &data[..])),
            &data,
        );
        boundaries(
            &name,
            &|d: &[u8]| StdReader::new(BufReader::with_capacity(capacity, d)),
            &data,
        );
    }
}

#[test]
fn conformance_depth_aware() {
    let data = data();
    happy(
        "DepthAwareReader<&[u8]>",
        DepthAwareReader::from_reader(&data[..]),
        &data,
    );
    boundaries_all(
        "DepthAwareReader<&[u8]>",
        &|d: &[u8]| DepthAwareReader::from_reader(d),
        &data,
    );
    until_all(
        "DepthAwareReader<&[u8]>",
        &|d: &[u8]| DepthAwareReader::from_reader(d),
        &data,
    );

    let name = "DepthAwareReader<StdReader>";
    happy(
        name,
        DepthAwareReader::from_reader(StdReader::new(BufReader::new(&data[..]))),
        &data,
    );
    boundaries_all(
        name,
        &|d: &[u8]| DepthAwareReader::from_reader(StdReader::new(BufReader::new(d))),
        &data,
    );
    until_all(
        name,
        &|d: &[u8]| DepthAwareReader::from_reader(StdReader::new(BufReader::new(d))),
        &data,
    );
}

#[test]
fn conformance_take() {
    let data = data();
    happy("Take<&[u8]>", Take::new(&data[..], data.len()), &data);
    boundaries_all("Take<&[u8]>", &|d: &[u8]| Take::new(d, d.len()), &data);
    until_all("Take<&[u8]>", &|d: &[u8]| Take::new(d, d.len()), &data);

    let name = "Take<StdReader>";
    happy(
        name,
        Take::new(
            StdReader::new(BufReader::with_capacity(3, &data[..])),
            data.len(),
        ),
        &data,
    );
    boundaries_all(
        name,
        &|d: &[u8]| Take::new(StdReader::new(BufReader::with_capacity(3, d)), d.len()),
        &data,
    );
    until_all(
        name,
        &|d: &[u8]| Take::new(StdReader::new(BufReader::with_capacity(3, d)), d.len()),
        &data,
    );
}

#[test]
fn conformance_forwarding_impls() {
    let data = data();

    let mut inner: &[u8] = &data;
    happy("&mut &[u8]", &mut inner, &data);

    happy("Box<&[u8]>", Box::new(&data[..]), &data);
    boundaries_all("Box<&[u8]>", &|d: &[u8]| Box::new(d), &data);
    until_all("Box<&[u8]>", &|d: &[u8]| Box::new(d), &data);

    {
        let mut inner: &[u8] = &data;
        let reader = &mut inner;
        reader.advance(data.len()).unwrap();
        assert_eof("&mut &[u8]", "peek_byte at EOF", reader.peek_byte());
    }
    {
        let mut inner: &[u8] = &data;
        let reader = &mut inner;
        reader.advance(data.len()).unwrap();
        assert_eof("&mut &[u8]", "read_byte at EOF", reader.read_byte());
    }
    {
        let mut inner: &[u8] = &data;
        let reader = &mut inner;
        reader.advance(data.len()).unwrap();
        assert_eof("&mut &[u8]", "advance(1) at EOF", reader.advance(1));
    }
    {
        let mut inner: &[u8] = &data;
        let reader = &mut inner;
        reader.advance(data.len()).unwrap();
        assert_slice(
            "&mut &[u8]",
            "read_slice(0) at EOF",
            reader.read_slice(0),
            &[],
        );
    }
    {
        let mut inner: &[u8] = &data;
        let reader = &mut inner;
        reader.advance(data.len()).unwrap();
        assert_slice(
            "&mut &[u8]",
            "read_till_eof at EOF",
            reader.read_till_eof(),
            &[],
        );
    }
    {
        let mut inner: &[u8] = &data;
        let reader = &mut inner;
        let (head, found) = reader.read_until_match(&[0x80u8]).unwrap();
        assert_eq!(
            (&head[..], found),
            (&data[..0x80], true),
            "&mut &[u8]: read_until(0x80)"
        );
    }
}

// --- `is_eof` / `remaining` --------------------------------------------------------------

/// The two observations at the current state, plus the invariants that hold every time they are
/// asked: asking twice is stable, and a byte that is still peekable is never reported as EOF.
fn observe<'d, R: Read<'d>>(name: &str, reader: &mut R, at_eof: bool, remaining: Option<usize>) {
    let eof = reader
        .is_eof()
        .unwrap_or_else(|e| panic!("{name}: is_eof: {e:?}"));
    assert_eq!(eof, at_eof, "{name}: is_eof");
    assert_eq!(reader.remaining(), remaining, "{name}: remaining");

    let eof_again = reader
        .is_eof()
        .unwrap_or_else(|e| panic!("{name}: is_eof (again): {e:?}"));
    assert_eq!(eof_again, at_eof, "{name}: is_eof must be stable");
    assert_eq!(
        reader.remaining(),
        remaining,
        "{name}: remaining must be stable"
    );

    if reader.peek_byte().is_ok() {
        assert!(!at_eof, "{name}: peek_byte is Ok while is_eof reported EOF");
    }
}

/// Every state a reader can be observed in: fresh, mid-stream, exactly at EOF, after a failed
/// read at EOF, and on an empty stream. `knows` is false for the readers that cannot answer
/// `remaining` (`StdReader`, and wrappers over it).
fn observe_all_states<'d, R: Read<'d>>(
    name: &str,
    mk: &impl Fn(&'d [u8]) -> R,
    data: &'d [u8],
    knows: bool,
) {
    let len = data.len();

    {
        let mut reader = mk(data);
        observe(name, &mut reader, false, knows.then_some(len));
    }
    {
        let mut reader = mk(data);
        reader
            .advance(len / 2)
            .unwrap_or_else(|e| panic!("{name}: advance(len/2): {e:?}"));
        observe(name, &mut reader, false, knows.then_some(len - len / 2));
    }
    {
        let mut reader = mk(data);
        reader
            .advance(len)
            .unwrap_or_else(|e| panic!("{name}: advance(len): {e:?}"));
        observe(name, &mut reader, true, knows.then_some(0));
    }
    {
        // A failed read at EOF must not move the answer.
        let mut reader = mk(data);
        reader.advance(len).unwrap();
        assert_eof(name, "read_byte at EOF", reader.read_byte());
        observe(name, &mut reader, true, knows.then_some(0));
    }
    {
        let empty = &data[..0];
        let mut reader = mk(empty);
        observe(name, &mut reader, true, knows.then_some(0));
    }
}

#[test]
fn conformance_eof_and_remaining_states() {
    let data = data();
    let d = &data[..];

    observe_all_states("&[u8]", &|d: &[u8]| d, d, true);
    observe_all_states("Vec<u8>", &|d: &[u8]| d.to_vec(), d, true);
    observe_all_states(
        "VecDeque<u8>",
        &|d: &[u8]| VecDeque::from(d.to_vec()),
        d,
        true,
    );
    observe_all_states("Cow::Borrowed", &|d: &[u8]| Cow::Borrowed(d), d, true);
    observe_all_states("Cow::Owned", &|d: &[u8]| Cow::Owned(d.to_vec()), d, true);
    observe_all_states(
        "DepthAwareReader<&[u8]>",
        &DepthAwareReader::from_reader,
        d,
        true,
    );
    observe_all_states(
        "StdReader",
        &|d: &[u8]| StdReader::new(BufReader::with_capacity(4, d)),
        d,
        false,
    );
    observe_all_states(
        "DepthAwareReader<StdReader>",
        &|d: &[u8]| DepthAwareReader::from_reader(StdReader::new(BufReader::with_capacity(4, d))),
        d,
        false,
    );
    observe_all_states("Box<&[u8]>", &|d: &[u8]| Box::new(d), d, true);
}

/// The depth wrappers must not change either answer: the same underlying stream, the same state,
/// the same verdict — including from inside a guard.
#[test]
fn conformance_eof_under_a_depth_guard() {
    let data = data();
    let d = &data[..];

    let mut reader = DepthAwareReader::from_reader(d);
    let mut guard = reader.enter().unwrap();
    observe("guard, fresh", &mut guard, false, Some(d.len()));

    guard.advance(d.len()).unwrap();
    observe("guard, drained", &mut guard, true, Some(0));

    // And a guard over a streaming inner keeps reporting "unknown".
    let mut reader = DepthAwareReader::from_reader(StdReader::new(BufReader::with_capacity(2, d)));
    let mut guard = reader.enter().unwrap();
    observe("guard over StdReader", &mut guard, false, None);
}

/// The blanket `&mut R` impl forwards both observations.
#[test]
fn conformance_eof_forwards_through_mut_ref() {
    let data = data();
    let mut inner: &[u8] = &data;
    let reader = &mut inner;

    assert!(!reader.is_eof().unwrap(), "&mut R: is_eof forwards");
    assert_eq!(
        reader.remaining(),
        Some(data.len()),
        "&mut R: remaining forwards"
    );

    reader.advance(data.len()).unwrap();
    assert!(reader.is_eof().unwrap(), "&mut R: at EOF");
    assert_eq!(reader.remaining(), Some(0), "&mut R: drained");
}

/// `Take` is EOF when its budget is spent *or* the inner stream is done, and `remaining` is what
/// the inner surely has, capped by the budget (`None` when the inner can't tell).
#[test]
fn conformance_take_eof_answers_for_limit_and_inner() {
    let data = data();
    let d = &data[..];

    // Budget spent, inner still has plenty.
    let mut take = Take::new(d, 4);
    take.advance(4).unwrap();
    observe("Take(slice, budget spent)", &mut take, true, Some(0));
    assert_eof("Take(slice, budget spent)", "read_byte", take.read_byte());

    // Budget left, inner exhausted: EOF, nothing left to promise.
    let mut take = Take::new(d, data.len() + 10);
    take.advance(data.len()).unwrap();
    observe("Take(slice, inner exhausted)", &mut take, true, Some(0));
    assert_eof(
        "Take(slice, inner exhausted)",
        "read_byte",
        take.read_byte(),
    );

    // Budget larger than the stream: `remaining` is what the stream has.
    let take = Take::new(d, data.len() + 10);
    assert_eq!(Read::remaining(&take), Some(data.len()));

    // Limit 0 over a non-empty stream.
    let mut take = Take::new(d, 0);
    observe("Take(slice, limit 0)", &mut take, true, Some(0));

    // Over a `StdReader`, whose own `remaining` is unknown.
    let mut take = Take::new(StdReader::new(BufReader::with_capacity(2, d)), 100);
    observe("Take(StdReader, fresh)", &mut take, false, None);
    take.advance(100).unwrap();
    observe("Take(StdReader, budget spent)", &mut take, true, None);

    let mut take = Take::new(
        StdReader::new(BufReader::with_capacity(2, d)),
        data.len() + 10,
    );
    take.advance(data.len()).unwrap();
    observe("Take(StdReader, inner exhausted)", &mut take, true, None);

    // `read_till_eof` returns what's left instead of failing on a short stream.
    let mut take = Take::new(
        StdReader::new(BufReader::with_capacity(2, d)),
        data.len() + 10,
    );
    assert_eq!(&*take.read_till_eof().unwrap(), d);
    let mut take = Take::new(d, data.len() + 10);
    assert_eq!(&*take.read_till_eof().unwrap(), d);
}

/// `Take` keeps its inherent `remaining() -> usize` (the budget) next to the trait method
/// returning `Option<usize>`: inherent resolution wins, so the two are reached differently and
/// silently differ in type.
#[test]
fn conformance_take_remaining_has_two_forms() {
    let data = data();
    let mut take = Take::new(&data[..], 7);
    take.advance(2).unwrap();

    assert_eq!(take.remaining(), 5, "inherent: the budget, as usize");
    assert_eq!(
        Read::remaining(&take),
        Some(5),
        "trait: the same budget, as Option"
    );
    assert!(!take.is_exhausted(), "budget left");
}

/// Draining the stream leaves `remaining` at exactly zero, for the impls that can answer.
#[test]
fn conformance_remaining_is_zero_after_drain() {
    let data = data();
    let d = &data[..];

    {
        let mut reader = d;
        assert_slice("&[u8]", "read_till_eof", reader.read_till_eof(), d);
        observe("&[u8] drained", &mut reader, true, Some(0));
    }
    {
        let mut reader = DepthAwareReader::from_reader(d);
        assert_slice(
            "DepthAwareReader",
            "read_till_eof",
            reader.read_till_eof(),
            d,
        );
        observe("DepthAwareReader drained", &mut reader, true, Some(0));
    }
    {
        // A budget that matches the stream is reached exactly by draining it.
        let mut take = Take::new(d, d.len());
        assert_slice("Take(slice, len)", "read_till_eof", take.read_till_eof(), d);
        observe("Take drained", &mut take, true, Some(0));
    }
}

/// `StdReader::is_eof` goes through the buffered refill: it populates the window, and the stream
/// is left where it was.
#[test]
fn conformance_std_is_eof_fills_without_consuming() {
    let data = data();
    let mut reader = StdReader::new(BufReader::with_capacity(4, &data[..]));

    assert!(reader.peek_slice().is_empty(), "nothing buffered yet");
    assert!(!reader.is_eof().unwrap(), "the stream has data");
    assert_eq!(
        reader.peek_slice().first().copied(),
        Some(data[0]),
        "is_eof refilled the window"
    );
    assert_eq!(reader.read_byte().unwrap(), data[0], "…without consuming");
}

/// `is_eof` answers about EOF only: a transient `Interrupted` is retried, and any other error is
/// reported rather than turned into an EOF verdict.
#[test]
fn conformance_std_is_eof_error_handling() {
    struct FailOnce {
        kind: ErrorKind,
        data: Vec<u8>,
        armed: bool,
    }

    impl std::io::Read for FailOnce {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.armed {
                self.armed = true;
                return Err(self.kind.into());
            }
            let n = buf.len().min(self.data.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data.drain(..n);
            Ok(n)
        }
    }

    let data = data();

    let mut reader = StdReader::new(BufReader::new(FailOnce {
        kind: ErrorKind::Interrupted,
        data: data.clone(),
        armed: false,
    }));
    assert!(
        !reader.is_eof().unwrap(),
        "Interrupted is transient: retried, not surfaced"
    );
    assert_eq!(reader.read_byte().unwrap(), data[0]);

    let mut reader = StdReader::new(BufReader::new(FailOnce {
        kind: ErrorKind::WouldBlock,
        data: data.clone(),
        armed: false,
    }));
    match reader.is_eof() {
        Err(ReadError::IoError(ErrorKind::WouldBlock)) => {}
        other => panic!("WouldBlock must propagate, got {other:?}"),
    }
}

/// Observing must not change what a script sees: the same steps, with and without probes
/// interleaved, produce the same trace.
#[test]
fn conformance_eof_probes_are_transparent() {
    fn trace<'d, R: Read<'d>>(mut reader: R, probing: bool) -> Vec<String> {
        let mut log = Vec::new();

        for step in 0..10 {
            if probing {
                // Ask both and drop the answers: the compared trace is the reads', and the claim
                // is that asking changed nothing about them.
                let _ = reader.is_eof();
                let _ = reader.remaining();
            }
            log.push(match step % 4 {
                0 => format!("peek {:?}", reader.peek_byte()),
                1 => format!("byte {:?}", reader.read_byte()),
                2 => format!("adv {:?}", reader.advance(3)),
                _ => format!("slice {:?}", reader.read_slice(5).map(|b| b.len())),
            });
        }

        log
    }

    let data = data();
    let d = &data[..];

    assert_eq!(trace(d, false), trace(d, true), "&[u8]: probing changed it");
    assert_eq!(
        trace(d.to_vec(), false),
        trace(d.to_vec(), true),
        "Vec<u8>: probing changed it"
    );
    assert_eq!(
        trace(Cow::Borrowed(d), false),
        trace(Cow::Borrowed(d), true),
        "Cow: probing changed it"
    );
    assert_eq!(
        trace(DepthAwareReader::from_reader(d), false),
        trace(DepthAwareReader::from_reader(d), true),
        "DepthAwareReader: probing changed it"
    );

    let std_reader = || StdReader::new(BufReader::with_capacity(3, d));
    assert_eq!(
        trace(std_reader(), false),
        trace(std_reader(), true),
        "StdReader: probing changed it"
    );

    let take = || Take::new(d, 12);
    assert_eq!(
        trace(take(), false),
        trace(take(), true),
        "Take: probing changed it"
    );

    let take_std = || Take::new(StdReader::new(BufReader::with_capacity(3, d)), 12);
    assert_eq!(
        trace(take_std(), false),
        trace(take_std(), true),
        "Take<StdReader>: probing changed it"
    );
}
