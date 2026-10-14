use std::borrow::Cow;
use std::io::{BufReader, ErrorKind};

use parsio::{DepthAwareReader, Read, StdReader, Write, WriteError};
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Op {
    Peek,
    ReadByte,
    Advance(usize),
    ReadSlice(usize),
    ReadArray1,
    ReadArray2,
    ReadArray4,
    ReadTillEof,
    IsEof,
    Remaining,
    ReadUntil(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Byte(u8),
    Bytes(Vec<u8>),
    /// `read_until`: bytes before the match (or the rest at EOF), and whether it matched.
    Until(Vec<u8>, bool),
    Advanced,
    /// `is_eof`
    Bool(bool),
    /// `remaining`: what the impl answered, `None` when it cannot know
    Remaining(Option<usize>),
    Err(ErrorKind),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => Just(Op::Peek),
        3 => Just(Op::ReadByte),
        2 => Just(Op::ReadArray1),
        2 => Just(Op::ReadArray2),
        2 => Just(Op::ReadArray4),
        2 => Just(Op::ReadTillEof),
        2 => Just(Op::IsEof),
        2 => Just(Op::Remaining),
        4 => length().prop_map(Op::Advance),
        4 => length().prop_map(Op::ReadSlice),
        4 => needle().prop_map(Op::ReadUntil),
    ]
}

/// Needles are drawn from a small alphabet so that they keep showing up in tiny random buffers,
/// including split across a refill (a two-byte needle and a one-byte buffer).
fn needle() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        6 => prop::collection::vec(0..2u8, 1..4),
        1 => prop::collection::vec(any::<u8>(), 1..6),
    ]
}

/// Lengths cluster around the data length boundaries (`0`, near-`len`, `len + 1`) with the
/// occasional absurd value, which is what catches unbounded allocation.
fn length() -> impl Strategy<Value = usize> {
    prop_oneof![
        4 => 0..12usize,
        2 => 90..104usize,
        1 => Just(usize::MAX / 2),
        1 => any::<usize>(),
    ]
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(op(), 0..24)
}

fn data() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..32)
}

/// Data over a two-symbol alphabet: `needle()` draws from the same one, so matches are dense and
/// land on every alignment, including the seams between refills.
fn dense_data() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(0..2u8, 0..32)
}

/// Reference evaluation of a script, stopping at the first error.
fn model(data: &[u8], ops: &[Op]) -> Vec<Step> {
    model_bounded(data, ops, data.len())
}

/// `StdReader` never knows how much is left, so it answers `None` wherever the model knows the
/// count; every other step of the script must still match exactly.
fn with_unknown_remaining(steps: &[Step]) -> Vec<Step> {
    steps
        .iter()
        .map(|step| match step {
            Step::Remaining(_) => Step::Remaining(None),
            other => other.clone(),
        })
        .collect()
}

/// Same, for a stream behind a `Take` limit of `limit` bytes. Operations needing more than what is
/// left under the limit fail whole, and nothing is handed out once it is reached.
fn model_bounded(data: &[u8], ops: &[Op], limit: usize) -> Vec<Step> {
    let len = data.len();
    let mut pos = 0usize;
    let mut remaining = limit;
    let mut steps = Vec::new();
    let eof = || Step::Err(ErrorKind::UnexpectedEof);

    for op in ops {
        let step = match op {
            Op::Peek => match remaining {
                0 => eof(),
                _ => match data.get(pos) {
                    Some(b) => Step::Byte(*b),
                    None => eof(),
                },
            },
            Op::ReadByte => match remaining {
                0 => eof(),
                _ => match data.get(pos) {
                    Some(b) => {
                        pos += 1;
                        remaining -= 1;
                        Step::Byte(*b)
                    }
                    None => eof(),
                },
            },
            Op::Advance(n) => {
                if *n > remaining {
                    eof()
                } else {
                    match pos.checked_add(*n).filter(|end| *end <= len) {
                        Some(end) => {
                            pos = end;
                            remaining -= n;
                            Step::Advanced
                        }
                        None => eof(),
                    }
                }
            }
            Op::ReadSlice(n) => {
                if *n > remaining {
                    eof()
                } else {
                    match pos.checked_add(*n).filter(|end| *end <= len) {
                        Some(end) => {
                            let bytes = data[pos..end].to_vec();
                            pos = end;
                            remaining -= n;
                            Step::Bytes(bytes)
                        }
                        None => eof(),
                    }
                }
            }
            Op::ReadArray1 => array_step::<1>(data, &mut pos, &mut remaining),
            Op::ReadArray2 => array_step::<2>(data, &mut pos, &mut remaining),
            Op::ReadArray4 => array_step::<4>(data, &mut pos, &mut remaining),
            Op::ReadTillEof => {
                // `Take::read_till_eof` asks for exactly what is left under the limit, so a limit
                // longer than the stream fails instead of returning what the stream has.
                if remaining > len - pos {
                    eof()
                } else {
                    let bytes = data[pos..pos + remaining].to_vec();
                    pos += remaining;
                    remaining = 0;
                    Step::Bytes(bytes)
                }
            }
            Op::IsEof => Step::Bool(remaining == 0),
            Op::Remaining => Step::Remaining(Some(remaining)),
            Op::ReadUntil(needle) => {
                // The window never reaches past the limit: a needle straddling its end cannot be
                // seen whole, so it is not found.
                let end = (pos + remaining).min(len);
                let found = if needle.len() == 1 {
                    data[pos..end].iter().position(|b| *b == needle[0])
                } else {
                    data[pos..end]
                        .windows(needle.len())
                        .position(|window| window == needle.as_slice())
                };
                match found {
                    Some(index) => {
                        let bytes = data[pos..pos + index].to_vec();
                        pos += index + needle.len();
                        remaining -= index + needle.len();
                        Step::Until(bytes, true)
                    }
                    None => {
                        let bytes = data[pos..end].to_vec();
                        remaining -= end - pos;
                        pos = end;
                        Step::Until(bytes, false)
                    }
                }
            }
        };

        let stop = matches!(step, Step::Err(_));
        steps.push(step);
        if stop {
            break;
        }
    }

    steps
}

fn array_step<const N: usize>(data: &[u8], pos: &mut usize, remaining: &mut usize) -> Step {
    if N > *remaining {
        return Step::Err(ErrorKind::UnexpectedEof);
    }
    match pos
        .checked_add(N)
        .filter(|end| *end <= data.len())
        .map(|end| (*pos, end))
    {
        Some((start, end)) => {
            *pos = end;
            *remaining -= N;
            Step::Bytes(data[start..end].to_vec())
        }
        None => Step::Err(ErrorKind::UnexpectedEof),
    }
}

/// Runs a script against one reader implementation.
fn run<'d, R: Read<'d>>(mk: impl Fn(&'d [u8]) -> R, data: &'d [u8], ops: &[Op]) -> Vec<Step> {
    run_reader(mk(data), data, ops)
}

/// Runs a script against one reader implementation behind a `Take` limit.
fn run_bounded<'d, R: Read<'d>>(
    mk: impl Fn(&'d [u8]) -> R,
    data: &'d [u8],
    ops: &[Op],
    limit: usize,
) -> Vec<Step> {
    run_reader(parsio::Take::new(mk(data), limit), data, ops)
}

fn run_reader<'d, R: Read<'d>>(mut reader: R, _data: &'d [u8], ops: &[Op]) -> Vec<Step> {
    let mut steps = Vec::new();

    for op in ops {
        let step = match *op {
            Op::Peek => match reader.peek_byte() {
                Ok(b) => Step::Byte(b),
                Err(e) => Step::Err(kind_of(e)),
            },
            Op::ReadByte => match reader.read_byte() {
                Ok(b) => Step::Byte(b),
                Err(e) => Step::Err(kind_of(e)),
            },
            Op::Advance(n) => match reader.advance(n) {
                Ok(()) => Step::Advanced,
                Err(e) => Step::Err(kind_of(e)),
            },
            Op::ReadSlice(n) => match reader.read_slice(n) {
                Ok(bytes) => Step::Bytes(bytes.to_vec()),
                Err(e) => Step::Err(kind_of(e)),
            },
            Op::ReadArray1 => match reader.read_array::<1>() {
                Ok(bytes) => Step::Bytes(bytes.to_vec()),
                Err(e) => Step::Err(kind_of(e)),
            },
            Op::ReadArray2 => match reader.read_array::<2>() {
                Ok(bytes) => Step::Bytes(bytes.to_vec()),
                Err(e) => Step::Err(kind_of(e)),
            },
            Op::ReadArray4 => match reader.read_array::<4>() {
                Ok(bytes) => Step::Bytes(bytes.to_vec()),
                Err(e) => Step::Err(kind_of(e)),
            },
            Op::ReadTillEof => match reader.read_till_eof() {
                Ok(bytes) => Step::Bytes(bytes.to_vec()),
                Err(e) => Step::Err(kind_of(e)),
            },
            Op::IsEof => match reader.is_eof() {
                Ok(b) => Step::Bool(b),
                Err(e) => Step::Err(kind_of(e)),
            },
            Op::Remaining => Step::Remaining(reader.remaining()),
            Op::ReadUntil(ref needle) => match reader.read_until(needle) {
                Ok((bytes, found)) => Step::Until(bytes.to_vec(), found),
                Err(e) => Step::Err(kind_of(e)),
            },
        };

        let stop = matches!(step, Step::Err(_));
        steps.push(step);
        if stop {
            break;
        }
    }

    steps
}

fn kind_of(e: parsio::ReadError) -> ErrorKind {
    match e {
        parsio::ReadError::IoError(kind) => kind,
        other => panic!("unexpected error variant: {other:?}"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        ..ProptestConfig::default()
    })]

    #[test]
    #[cfg_attr(miri, ignore)]
    fn every_reader_impl_agrees_with_the_model(
        data in data(),
        ops in ops(),
        needle in needle(),
        cap in 1..=16usize
    ) {
        // Scripts die at their first error, and their ops are mostly destructive, so a scan is put
        // first: that way every case actually executes one, on a reader that has consumed nothing.
        let mut ops = ops;
        ops.insert(0, Op::ReadUntil(needle));

        let expect = model(&data, &ops);
        let expect_unknown = with_unknown_remaining(&expect);
        let data = data.as_slice();

        prop_assert_eq!(run(|d| d, data, &ops), expect.clone(), "&[u8]");
        prop_assert_eq!(run(|d: &[u8]| d.to_vec(), data, &ops), expect.clone(), "Vec<u8>");
        prop_assert_eq!(
            run(|d: &[u8]| Cow::Borrowed(d), data, &ops),
            expect.clone(),
            "Cow::Borrowed"
        );
        prop_assert_eq!(
            run(|d: &[u8]| Cow::Owned(d.to_vec()), data, &ops),
            expect.clone(),
            "Cow::Owned"
        );
        prop_assert_eq!(
            run(|d: &[u8]| StdReader::new(BufReader::with_capacity(cap, d)), data, &ops),
            expect_unknown.clone(),
            "StdReader"
        );
        prop_assert_eq!(
            run(DepthAwareReader::from_reader, data, &ops),
            expect.clone(),
            "DepthAwareReader<&[u8]>"
        );
        prop_assert_eq!(
            run(
                |d: &[u8]| DepthAwareReader::from_reader(StdReader::new(BufReader::with_capacity(cap, d))),
                data,
                &ops
            ),
            expect_unknown,
            "DepthAwareReader<StdReader>"
        );
    }

    /// `Take` enforces its limit itself — the inner readers' scans would run past it — so the whole
    /// script has to hold under it too. The tiny capacities make windows shorter than a needle, and
    /// windows that drain between refills, which is where seam handling lives.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn take_limit_agrees_with_the_model(
        data in dense_data(),
        ops in ops(),
        needle in needle(),
        cap in 1..=4usize,
        limit in 0..=40usize
    ) {
        let mut ops = ops;
        ops.insert(0, Op::ReadUntil(needle));

        let expect = model_bounded(&data, &ops, limit);
        let data = data.as_slice();

        prop_assert_eq!(run_bounded(|d| d, data, &ops, limit), expect.clone(), "Take<&[u8]>");
        prop_assert_eq!(
            run_bounded(|d: &[u8]| d.to_vec(), data, &ops, limit),
            expect.clone(),
            "Take<Vec<u8>>"
        );
        prop_assert_eq!(
            run_bounded(|d: &[u8]| Cow::Borrowed(d), data, &ops, limit),
            expect.clone(),
            "Take<Cow::Borrowed>"
        );
        prop_assert_eq!(
            run_bounded(|d: &[u8]| Cow::Owned(d.to_vec()), data, &ops, limit),
            expect.clone(),
            "Take<Cow::Owned>"
        );
        prop_assert_eq!(
            run_bounded(
                |d: &[u8]| StdReader::new(BufReader::with_capacity(cap, d)),
                data,
                &ops,
                limit
            ),
            expect.clone(),
            "Take<StdReader>"
        );
        prop_assert_eq!(
            run_bounded(DepthAwareReader::from_reader, data, &ops, limit),
            expect.clone(),
            "Take<DepthAwareReader<&[u8]>>"
        );
        prop_assert_eq!(
            run_bounded(
                |d: &[u8]| DepthAwareReader::from_reader(StdReader::new(BufReader::with_capacity(cap, d))),
                data,
                &ops,
                limit
            ),
            expect,
            "Take<DepthAwareReader<StdReader>>"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn slice_write_all_is_all_or_nothing(input in prop::collection::vec(any::<u8>(), 0..40), cap in 0..24usize) {
        let mut buf = vec![0xffu8; cap];

        let result = {
            let mut w: &mut [u8] = &mut buf;
            w.write_all(&input)
        };

        if input.len() <= cap {
            prop_assert!(result.is_ok(), "expected Ok for {} bytes into {cap}", input.len());
            prop_assert_eq!(&buf[..input.len()], input.as_slice());
        } else {
            prop_assert!(
                matches!(result, Err(WriteError::IoError(ErrorKind::WriteZero))),
                "expected WriteZero, got {result:?}"
            );
            prop_assert!(buf.iter().all(|b| *b == 0xff), "buffer must be untouched");
        }
    }
}
