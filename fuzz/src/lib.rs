//! Shared fuzzing scaffolding: input decoding, the model oracle, and one entry point per fuzz
//! target.
//!
//! The entry points live here (not in the targets) so that `tests::corpus_replay` can drive the
//! committed corpus files through exactly the same code, on stable and under Miri, where libFuzzer
//! itself cannot run.
//!
//! Contract rule that shapes every oracle: on `Err`, the amount consumed by a reader is
//! unspecified (see `parsio::Read`), so comparisons stop at the first error.

use std::alloc::{GlobalAlloc, Layout, System};
use std::borrow::Cow;
use std::io::{BufReader, ErrorKind};
use std::sync::atomic::{AtomicUsize, Ordering};

use arbitrary::Arbitrary;

pub use parsio::{DepthAwareReader as DepthReader, Read, StdReader, StdWriter, Take, Write};

/// Tracks peak live bytes, so a target can fail when an operation holds more than the input can
/// justify (`read_slice(len)` pre-allocating `len` used to abort the process).
pub struct CountingAlloc;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static BASE: AtomicUsize = AtomicUsize::new(0);

fn grew(by: usize) {
    let live = LIVE.fetch_add(by, Ordering::Relaxed) + by;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            grew(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            match new_size.checked_sub(layout.size()) {
                Some(by) => grew(by),
                None => _ = LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed),
            }
        }
        new
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Peak live bytes since the last [`reset_allocated`]
pub fn allocated_bytes() -> usize {
    PEAK.load(Ordering::Relaxed)
        .saturating_sub(BASE.load(Ordering::Relaxed))
}

pub fn reset_allocated() {
    let live = LIVE.load(Ordering::Relaxed);
    BASE.store(live, Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
}

/// Lengths are relative to the buffer under test, so shrinking stays meaningful.
#[derive(Arbitrary, Debug, Clone, Copy)]
pub enum Length {
    Zero,
    One,
    Exact,
    Over,
    Huge,
    Absurd,
}

impl Length {
    fn resolve(self, len: usize) -> usize {
        match self {
            Length::Zero => 0,
            Length::One => 1,
            Length::Exact => len,
            Length::Over => len.saturating_add(1),
            Length::Huge => usize::MAX / 2,
            Length::Absurd => usize::MAX,
        }
    }
}

#[derive(Arbitrary, Debug, Clone)]
pub enum Op {
    Peek,
    ReadByte,
    Advance(Length),
    ReadSlice(Length),
    ReadArray1,
    ReadArray2,
    ReadArray4,
    ReadTillEof,
    /// `is_eof`
    IsEof,
    /// `remaining`
    Remaining,
    /// `read_until` with this many needle bytes, taken from the head of the input buffer (0 means
    /// "the whole buffer as the needle").
    ReadUntil(u8),
}

/// The needle for `Op::ReadUntil(n)`: `n` bytes off the front of the data, never empty. Borrowed:
/// the harness computes needles once per input, and the allocation budget below the fuzz target
/// must not count them repeatedly.
fn needle_for(data: &[u8], n: u8) -> &[u8] {
    if data.is_empty() {
        return &[0];
    }
    let len = match n {
        0 => data.len(),
        n => (n as usize).min(data.len()),
    };
    &data[..len.max(1)]
}

#[derive(Arbitrary, Debug)]
pub struct ReadInput {
    pub buf: Vec<u8>,
    pub ops: Vec<Op>,
    pub capacity: u8,
    /// `Take` limit for the bounded cells. Left as an absolute byte count (not resolved against the
    /// buffer) so that limits shorter, equal to, and longer than the stream all get exercised.
    pub take_limit: u8,
}

impl ReadInput {
    /// `BufReader` capacity, deliberately tiny so every refill path is hit.
    pub fn capacity(&self) -> usize {
        (self.capacity % 8) as usize + 1
    }
}

#[derive(Arbitrary, Debug)]
pub struct HostileInput {
    pub read: ReadInput,
    /// Per-read-call schedule driving chunk sizes and error injection.
    pub schedule: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Byte(u8),
    Bytes(Vec<u8>),
    /// `read_until` result: bytes before the match (or the rest at EOF), plus whether it matched.
    Until(Vec<u8>, bool),
    Advanced,
    /// `is_eof`
    Bool(bool),
    /// `remaining`: `None` when the impl cannot know
    Remaining(Option<usize>),
    Err(ErrorKind),
}

/// Reference evaluation of a script, stopping at the first error.
pub fn model(data: &[u8], ops: &[Op]) -> Vec<Step> {
    model_bounded(data, ops, data.len())
}

/// `StdReader` never knows how much is left, so it answers `None` wherever the model knows the
/// count; every other step of the script must still match exactly.
pub fn with_unknown_remaining(steps: &[Step]) -> Vec<Step> {
    steps
        .iter()
        .map(|step| match step {
            Step::Remaining(_) => Step::Remaining(None),
            other => other.clone(),
        })
        .collect()
}

/// Same, for a stream behind a `Take` limit of `limit` bytes: nothing is handed out once the limit
/// is reached, and operations that would need more than what is left under it fail whole (the
/// adapter checks before delegating, so no partial consumption happens on the way to an error).
pub fn model_bounded(data: &[u8], ops: &[Op], limit: usize) -> Vec<Step> {
    let len = data.len();
    let mut pos = 0usize;
    let mut remaining = limit;
    let mut steps = Vec::new();

    for op in ops {
        let step = match *op {
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
            Op::Advance(l) => {
                let n = l.resolve(len);
                if n > remaining {
                    eof()
                } else if advance_by(len, &mut pos, n) {
                    remaining -= n;
                    Step::Advanced
                } else {
                    eof()
                }
            }
            Op::ReadSlice(l) => {
                let n = l.resolve(len);
                if n > remaining {
                    eof()
                } else {
                    match take(data, &mut pos, n) {
                        Some(bytes) => {
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
                // Whatever is left under the limit, fewer bytes if the stream ends first
                let end = (pos + remaining).min(len);
                let bytes = data[pos..end].to_vec();
                remaining -= end - pos;
                pos = end;
                Step::Bytes(bytes)
            }
            // EOF when the budget is spent or the stream is done
            Op::IsEof => Step::Bool(remaining == 0 || pos == len),
            // What the stream surely has, capped by the budget
            Op::Remaining => Step::Remaining(Some(remaining.min(len - pos))),
            Op::ReadUntil(n) => {
                let needle = needle_for(data, n);
                // The window never reaches past the limit, so a needle straddling its end is not
                // found -- it cannot be seen whole.
                let end = (pos + remaining).min(len);
                match find(&data[pos..end], needle) {
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
            return steps;
        }
    }

    steps
}

/// Reference substring search for the model.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() == 1 {
        haystack.iter().position(|byte| *byte == needle[0])
    } else {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}

fn eof() -> Step {
    Step::Err(ErrorKind::UnexpectedEof)
}

fn advance_by(len: usize, pos: &mut usize, n: usize) -> bool {
    match pos.checked_add(n).filter(|end| *end <= len) {
        Some(end) => {
            *pos = end;
            true
        }
        None => false,
    }
}

fn take(data: &[u8], pos: &mut usize, n: usize) -> Option<Vec<u8>> {
    let end = pos.checked_add(n).filter(|end| *end <= data.len())?;
    let bytes = data[*pos..end].to_vec();
    *pos = end;
    Some(bytes)
}

fn array_step<const N: usize>(data: &[u8], pos: &mut usize, remaining: &mut usize) -> Step {
    if N > *remaining {
        return eof();
    }
    match take(data, pos, N) {
        Some(bytes) => {
            *remaining -= N;
            Step::Bytes(bytes)
        }
        None => eof(),
    }
}

/// Runs a script against one reader implementation.
pub fn run<'d, R: Read<'d>>(mk: impl Fn(&'d [u8]) -> R, data: &'d [u8], ops: &[Op]) -> Vec<Step> {
    run_reader(mk(data), data, ops)
}

/// Runs a script against one reader implementation behind a `Take` limit.
pub fn run_bounded<'d, R: Read<'d>>(
    mk: impl Fn(&'d [u8]) -> R,
    data: &'d [u8],
    ops: &[Op],
    limit: usize,
) -> Vec<Step> {
    run_reader(Take::new(mk(data), limit), data, ops)
}

fn run_reader<'d, R: Read<'d>>(mut reader: R, data: &'d [u8], ops: &[Op]) -> Vec<Step> {
    let mut steps = Vec::new();

    for op in ops {
        let step = match *op {
            Op::Peek => step_of(reader.peek_byte().map(Step::Byte)),
            Op::ReadByte => step_of(reader.read_byte().map(Step::Byte)),
            Op::Advance(l) => match reader.advance(l.resolve(data.len())) {
                Ok(()) => Step::Advanced,
                Err(e) => Step::Err(kind_of(e)),
            },
            Op::ReadSlice(l) => step_of(reader.read_slice(l.resolve(data.len())).map(owned_slice)),
            Op::ReadArray1 => step_of(reader.read_array::<1>().map(owned_array::<1>)),
            Op::ReadArray2 => step_of(reader.read_array::<2>().map(owned_array::<2>)),
            Op::ReadArray4 => step_of(reader.read_array::<4>().map(owned_array::<4>)),
            Op::ReadTillEof => step_of(reader.read_till_eof().map(owned_slice)),
            Op::IsEof => match reader.is_eof() {
                Ok(b) => Step::Bool(b),
                Err(e) => Step::Err(kind_of(e)),
            },
            Op::Remaining => Step::Remaining(reader.remaining()),
            Op::ReadUntil(n) => match reader.read_until_match(needle_for(data, n)) {
                Ok((bytes, found)) => Step::Until(owned_slice(bytes).into_bytes(), found),
                Err(e) => Step::Err(kind_of(e)),
            },
        };

        let stop = matches!(step, Step::Err(_));
        steps.push(step);
        if stop {
            return steps;
        }
    }

    steps
}

fn step_of(r: Result<Step, parsio::ReadError>) -> Step {
    match r {
        Ok(step) => step,
        Err(e) => Step::Err(kind_of(e)),
    }
}

fn owned_slice(bytes: Cow<'_, [u8]>) -> Step {
    Step::Bytes(bytes.into_owned())
}

/// The same conversion, for the one step that also carries a flag.
trait IntoBytes {
    fn into_bytes(self) -> Vec<u8>;
}

impl IntoBytes for Step {
    fn into_bytes(self) -> Vec<u8> {
        match self {
            Step::Bytes(bytes) => bytes,
            other => panic!("expected Step::Bytes, got {other:?}"),
        }
    }
}

fn owned_array<const N: usize>(bytes: Cow<'_, [u8; N]>) -> Step {
    Step::Bytes(bytes.as_ref().to_vec())
}

fn kind_of(e: parsio::ReadError) -> ErrorKind {
    match e {
        parsio::ReadError::IoError(kind) => kind,
        other => panic!("unexpected error variant: {other:?}"),
    }
}

/// Target 1: every impl must agree with the model, and nothing may allocate out of proportion to
/// the input.
pub fn fuzz_read_ops(input: &ReadInput) {
    let data = input.buf.as_slice();
    let ops = input.ops.as_slice();
    let expect = model(data, ops);
    let expect_unknown = with_unknown_remaining(&expect);

    assert_eq!(run(|d| d, data, ops), expect, "&[u8]");
    assert_eq!(run(|d: &[u8]| d.to_vec(), data, ops), expect, "Vec<u8>");
    assert_eq!(
        run(
            |d: &[u8]| std::collections::VecDeque::from(d.to_vec()),
            data,
            ops
        ),
        expect,
        "VecDeque<u8>"
    );
    assert_eq!(
        run(|d: &[u8]| parsio::CowDeque::from(d.to_vec()), data, ops),
        expect,
        "CowDeque (owned)"
    );
    assert_eq!(
        run(|d: &[u8]| Cow::Borrowed(d), data, ops),
        expect,
        "Cow::Borrowed"
    );
    assert_eq!(
        run(|d: &[u8]| Cow::Owned(d.to_vec()), data, ops),
        expect,
        "Cow::Owned"
    );
    assert_eq!(
        run(|d: &[u8]| DepthReader::from_reader(d), data, ops),
        expect,
        "DepthAwareReader<&[u8]>"
    );

    let capacity = input.capacity();
    assert_eq!(
        run(
            |d: &[u8]| StdReader::new(BufReader::with_capacity(capacity, d)),
            data,
            ops
        ),
        expect_unknown,
        "StdReader"
    );

    // Under a `Take` limit the whole script must hold as well, and `Take` is the only thing
    // enforcing that limit: the inner readers' own scans would run past it. The tiny capacities
    // also make windows shorter than a needle, and windows that drain between refills.
    let limit = input.take_limit as usize;
    let expect_bounded = model_bounded(data, ops, limit);
    assert_eq!(
        run_bounded(|d| d, data, ops, limit),
        expect_bounded,
        "Take<&[u8]> (limit {limit})"
    );
    assert_eq!(
        run_bounded(|d: &[u8]| d.to_vec(), data, ops, limit),
        expect_bounded,
        "Take<Vec<u8>> (limit {limit})"
    );
    assert_eq!(
        run_bounded(
            |d: &[u8]| std::collections::VecDeque::from(d.to_vec()),
            data,
            ops,
            limit
        ),
        expect_bounded,
        "Take<VecDeque<u8>> (limit {limit})"
    );
    assert_eq!(
        run_bounded(parsio::CowDeque::from, data, ops, limit),
        expect_bounded,
        "Take<CowDeque> (borrowed) (limit {limit})"
    );
    assert_eq!(
        run_bounded(|d: &[u8]| Cow::Owned(d.to_vec()), data, ops, limit),
        expect_bounded,
        "Take<Cow::Owned> (limit {limit})"
    );
    assert_eq!(
        run_bounded(|d: &[u8]| DepthReader::from_reader(d), data, ops, limit),
        expect_bounded,
        "Take<DepthAwareReader<&[u8]>> (limit {limit})"
    );
    assert_eq!(
        run_bounded(
            |d: &[u8]| StdReader::new(BufReader::with_capacity(capacity, d)),
            data,
            ops,
            limit
        ),
        with_unknown_remaining(&expect_bounded),
        "Take<StdReader> (limit {limit})"
    );

    // `read_slice(huge)` used to allocate the requested length up front; now it's capped by the
    // 4MiB free reservation, plus slack for the harness itself
    let budget = 5 << 20;
    assert!(
        allocated_bytes() < budget,
        "allocated {} bytes, over the {budget} byte budget",
        allocated_bytes()
    );
}

/// Target 2: a hostile `io::Read` (tiny chunks, `Interrupted`, `WouldBlock`, hard errors) must not
/// change what the reader observes on the logical stream.
pub fn fuzz_std_reader_adversarial(input: &ReadInput, schedule: &[u8]) {
    let data = input.buf.as_slice();
    let ops = input.ops.as_slice();
    let expect_unknown = with_unknown_remaining(&model(data, ops));
    let capacity = input.capacity();

    // No errors injected: chunking alone must be invisible.
    let clean = run(
        |d: &[u8]| {
            StdReader::new(BufReader::with_capacity(
                capacity,
                Hostile::new(d, schedule, false),
            ))
        },
        data,
        ops,
    );
    assert_eq!(
        clean, expect_unknown,
        "chunked reader diverged from the model"
    );

    // Chunked and limited: `Take` has to scan this stream itself, over windows that drain between
    // refills.
    let limit = input.take_limit as usize;
    assert_eq!(
        run_bounded(
            |d: &[u8]| StdReader::new(BufReader::with_capacity(
                capacity,
                Hostile::new(d, schedule, false)
            )),
            data,
            ops,
            limit
        ),
        with_unknown_remaining(&model_bounded(data, ops, limit)),
        "Take<StdReader<chunked>> diverged from the model (limit {limit})"
    );

    // With errors injected, everything before the first error must still match, and the error
    // itself must be a legitimate kind.
    let hostile = run(
        |d: &[u8]| {
            StdReader::new(BufReader::with_capacity(
                capacity,
                Hostile::new(d, schedule, true),
            ))
        },
        data,
        ops,
    );
    let first_err = hostile
        .iter()
        .position(|step| matches!(step, Step::Err(_)))
        .unwrap_or(hostile.len());
    assert_eq!(
        hostile[..first_err],
        expect_unknown[..first_err.min(expect_unknown.len())],
        "reader diverged before its first error"
    );
    if let Some(Step::Err(kind)) = hostile.get(first_err) {
        assert!(
            matches!(
                kind,
                ErrorKind::UnexpectedEof
                    | ErrorKind::Interrupted
                    | ErrorKind::WouldBlock
                    | ErrorKind::Other
            ),
            "unexpected error kind: {kind:?}"
        );
    }
}

/// An `io::Read` that serves `data` in tiny chunks and, when asked, injects transient and hard
/// errors on a schedule derived from the input.
pub struct Hostile<'a> {
    data: &'a [u8],
    schedule: &'a [u8],
    errors: bool,
    pos: usize,
    tick: usize,
}

impl<'a> Hostile<'a> {
    pub fn new(data: &'a [u8], schedule: &'a [u8], errors: bool) -> Self {
        Self {
            data,
            schedule,
            errors,
            pos: 0,
            tick: 0,
        }
    }
}

impl std::io::Read for Hostile<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let step = self
            .schedule
            .get(self.tick % self.schedule.len().max(1))
            .copied()
            .unwrap_or(3);
        self.tick += 1;

        // At most one injected error per five calls, so the reader always makes progress. A reader
        // that failed every call would spin forever inside `read_to_end` (it retries
        // `Interrupted`), which is std's semantics, not a library bug — but a fuzz target must
        // still terminate.
        if self.errors && self.tick.is_multiple_of(5) {
            let kind = match step % 3 {
                0 => std::io::ErrorKind::Interrupted,
                1 => std::io::ErrorKind::WouldBlock,
                _ => std::io::ErrorKind::Other,
            };
            return Err(kind.into());
        }

        // 1..=8 bytes per call, so every refill boundary gets hit.
        let chunk = (step % 8) as usize + 1;
        let n = buf.len().min(chunk).min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Target 3: write invariants. `write_all` on a slice is all-or-nothing, and a sink never receives
/// anything but a prefix of the input.
pub fn fuzz_write_sinks(input: &WriteInput) {
    let capacity = input.capacity % 64;
    let chunks: Vec<&[u8]> = input.chunks.iter().map(|c| c.as_slice()).collect();
    let all: Vec<u8> = chunks.concat();

    // Fixed slice: short writes land in order, `write_all` is atomic.
    let mut buf = vec![0u8; capacity];
    let mut expected: Vec<u8> = Vec::new();
    {
        let mut w: &mut [u8] = &mut buf;

        for chunk in &chunks {
            let space = w.len();
            let written = Write::write(&mut w, chunk).expect("&mut [u8]: write");
            assert_eq!(
                written,
                chunk.len().min(space),
                "&mut [u8]: short write count"
            );
            expected.extend_from_slice(&chunk[..written]);
        }

        let remaining = w.len();
        let before = expected.clone();
        let result = Write::write_all(&mut w, &all);
        if all.len() > remaining {
            assert!(
                matches!(
                    result,
                    Err(parsio::WriteError::IoError(ErrorKind::WriteZero))
                ),
                "&mut [u8]: expected WriteZero for {} bytes into {remaining}",
                all.len()
            );
            assert_eq!(
                expected, before,
                "&mut [u8]: failed write_all changed the buffer"
            );
        } else {
            assert!(
                result.is_ok(),
                "&mut [u8]: write_all failed within capacity"
            );
            expected.extend_from_slice(&all);
        }
    }
    assert_eq!(
        &buf[..expected.len()],
        expected.as_slice(),
        "&mut [u8]: bytes landed out of order"
    );
    assert!(
        buf[expected.len()..].iter().all(|b| *b == 0),
        "&mut [u8]: bytes past the writes must be untouched"
    );

    // Vec sink: everything lands, counts add up.
    let mut sink: Vec<u8> = Vec::new();
    let mut total = 0usize;
    for chunk in &chunks {
        total += Write::write(&mut sink, chunk).expect("Vec<u8>: write");
    }
    assert_eq!(total, all.len(), "Vec<u8>: short write");
    assert_eq!(sink, all, "Vec<u8>: bytes landed");

    // Vectorized writes: the count must be the bytes actually landed and they must land in
    // order — every sink, same input. `&mut [u8]` stops at the buffer's end; `Vec` and
    // `StdWriter` take everything.
    if !chunks.is_empty() {
        let mut buf = vec![0u8; capacity];
        {
            let mut w: &mut [u8] = &mut buf;
            let n = Write::write_vectored(&mut w, &chunks).expect("&mut [u8]: write_vectored");
            assert!(n <= capacity, "&mut [u8]: count past the buffer end");
            assert_eq!(
                &buf[..n],
                &all[..n],
                "&mut [u8]: vectored bytes out of order"
            );
            assert!(
                buf[n..].iter().all(|b| *b == 0),
                "&mut [u8]: bytes past the vectored write must be untouched"
            );
        }

        let mut sink: Vec<u8> = Vec::new();
        let n = Write::write_vectored(&mut sink, &chunks).expect("Vec<u8>: write_vectored");
        assert_eq!(n, all.len(), "Vec<u8>: vectored short count");
        assert_eq!(sink, all, "Vec<u8>: vectored bytes landed wrong");

        let mut hostile = HostileWriter::default();
        {
            let mut writer = StdWriter::new(&mut hostile);
            let _ = Write::write_vectored(&mut writer, &chunks);
            let _ = Write::flush(&mut writer);
        }
        assert!(
            all.starts_with(&hostile.written),
            "StdWriter<hostile>: vectored write landed a non-prefix"
        );
    }

    // Hostile io::Write behind StdWriter: what lands is always a prefix of the input, even when
    // the writer fails part way through. Each case needs its own sink — writing the input twice
    // would make the prefix invariant meaningless.
    let mut hostile = HostileWriter::default();
    {
        let mut writer = StdWriter::new(&mut hostile);
        // `Write::write` may accept only part of a buffer, so keep feeding it the remainder;
        // stop at the first failure and the sink stays a prefix.
        'chunks: for chunk in &chunks {
            let mut rest: &[u8] = chunk;
            while !rest.is_empty() {
                match Write::write(&mut writer, rest) {
                    Ok(0) | Err(_) => break 'chunks,
                    Ok(n) => rest = &rest[n..],
                }
            }
        }
        let _ = Write::flush(&mut writer);
    }
    assert!(
        all.starts_with(&hostile.written),
        "StdWriter<hostile>: chunked writes landed a non-prefix"
    );

    let mut hostile = HostileWriter::default();
    {
        let mut writer = StdWriter::new(&mut hostile);
        let _ = Write::write_all(&mut writer, &all);
        let _ = Write::flush(&mut writer);
    }
    assert!(
        all.starts_with(&hostile.written),
        "StdWriter<hostile>: write_all landed a non-prefix"
    );

    let mut hostile = HostileWriter::default();
    {
        let mut writer = StdWriter::new(&mut hostile);
        let _ = std::io::Write::write_all(&mut writer, &all);
        let _ = std::io::Write::flush(&mut writer);
    }
    assert!(
        all.starts_with(&hostile.written),
        "std::io::Write::write_all landed a non-prefix"
    );
}

#[derive(Arbitrary, Debug)]
pub struct WriteInput {
    pub capacity: usize,
    pub chunks: Vec<Vec<u8>>,
}

/// Short writes and `Interrupted` on a fixed cycle, so `write_all`'s loop and the prefix invariant
/// both get exercised.
#[derive(Default)]
pub struct HostileWriter {
    pub written: Vec<u8>,
    tick: usize,
}

impl std::io::Write for HostileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.tick += 1;
        match self.tick % 7 {
            0 => return Err(std::io::ErrorKind::Interrupted.into()),
            1 if !buf.is_empty() => {
                self.written.push(buf[0]);
                return Ok(1);
            }
            _ => {}
        }
        let n = (buf.len() / 2).max(1).min(buf.len());
        self.written.extend_from_slice(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[derive(Arbitrary, Debug)]
pub enum DepthOp {
    /// Enter a level, reading the whole stream through the guard that claims it.
    EnterRead,
    /// Enter a level without reading, only claiming its depth.
    Enter,
    Drop,
    Forget,
}

#[derive(Arbitrary, Debug)]
pub struct DepthInput {
    pub limit: u8,
    pub ops: Vec<DepthOp>,
}

/// One segment of the script: `enters` nested levels (each optionally reading the stream
/// through its guard), then how the chain ends.
#[derive(Debug, PartialEq, Eq)]
struct Segment {
    enters: Vec<bool>,
    end: ChainEnd,
}

#[derive(Debug, PartialEq, Eq)]
enum ChainEnd {
    Drop,
    Forget,
    End,
}

/// Splits the op script into segments; `Drop`/`Forget` terminate a segment, and the implicit
/// end of the script terminates the last one. Recursion in `drive` is bounded by the depth
/// limit, not by segment length.
fn segments(ops: &[DepthOp]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut enters = Vec::new();

    let push_segment = |enters: &mut Vec<bool>, end: ChainEnd, out: &mut Vec<Segment>| {
        out.push(Segment {
            enters: enters.clone(),
            end,
        });
        enters.clear();
    };

    for op in ops {
        match op {
            DepthOp::Enter => enters.push(false),
            DepthOp::EnterRead => enters.push(true),
            DepthOp::Drop => push_segment(&mut enters, ChainEnd::Drop, &mut out),
            DepthOp::Forget => push_segment(&mut enters, ChainEnd::Forget, &mut out),
        }
    }
    push_segment(&mut enters, ChainEnd::End, &mut out);
    out
}

/// Target 4: depth accounting against a model that tracks the reader's counter, including levels
/// leaked by `mem::forget`.
///
/// Model: `depth` is what the reader holds, `leaked` are levels no guard owns any more. Every
/// `enter` targets `depth + 1`; on success the counter moves up, on overflow it must not move at
/// all — the historical bug was a counter that moved on the error path.
///
/// Since `enter` now borrows its receiver, a chain cannot be held in a local any more (the child
/// borrows the parent): each segment is driven by recursion, which is also how real parsers
/// consume a chain. Guards that read do so through the guard, asserting the stream is visible at
/// every claimed level.
pub fn fuzz_depth_accounting(input: &DepthInput) {
    let limit = (input.limit % 6) as usize;
    let data = b"abcdefgh";
    let mut reader = DepthReader::from_reader_with_limit(&data[..], limit as u16);

    let mut depth = 0usize;
    let mut leaked = 0usize;
    let mut forgets = 0usize;

    for Segment { enters, end } in segments(&input.ops) {
        assert_eq!(depth, leaked, "a segment must start with no live chain");

        let base = depth;
        if enters.is_empty() {
            continue;
        }

        match reader.enter() {
            Ok(mut root) => {
                depth = base + 1;
                // `drive` consumes the current flag itself before entering deeper, so the
                // root's own read flag is `enters[0]`, not `enters[1]`.
                drive(&mut root, &enters, data, base + 1, limit, &mut depth);

                match end {
                    ChainEnd::Forget if forgets < 4 => {
                        forgets += 1;
                        // The root is the only guard that survives `drive`'s frames — the
                        // children are locals there and drop as each frame returns — so
                        // forgetting the root leaks exactly one level.
                        leaked += 1;
                        depth = leaked;
                        std::mem::forget(root);
                    }
                    ChainEnd::Drop | ChainEnd::Forget | ChainEnd::End => {
                        drop(root);
                        depth = leaked;
                    }
                }
            }
            Err(e) => overflow_assert(e, base + 1, limit),
        }
    }

    // With every chain unwound, only the leaked levels remain: exactly `limit - leaked` more
    // levels must be enterable, and the next one must overflow.
    let available = limit.saturating_sub(leaked);
    for level in 1..=available + 1 {
        let outcome = reader.enter();
        if level <= available {
            let live = outcome.unwrap_or_else(|e| panic!("level {level} should fit: {e:?}"));
            std::mem::forget(live);
        } else {
            assert!(
                matches!(outcome, Err(parsio::ReadError::AllowedDepthOverflow { .. })),
                "level {level} past the leaked budget must overflow"
            );
        }
    }
}

/// Drives a segment's chain: recursion replaces the consuming-chain loop, the child borrows the
/// parent, and every level that must read sees the stream through its own guard. `level` is the
/// depth the current guard lives at; `depth` tracks the model's copy of the reader counter.
fn drive<'a, 'd>(
    guard: &mut parsio::DepthAwareReaderGuard<'a, &'d [u8]>,
    levels: &[bool],
    data: &'d [u8],
    level: usize,
    limit: usize,
    depth: &mut usize,
) {
    let Some(&reads) = levels.first() else {
        return;
    };

    if reads {
        let window = guard.peek_slice();
        assert!(
            !window.is_empty() && window == data,
            "level {level} must see the stream"
        );
        let _ = window;
    }

    let rest = &levels[1..];
    if rest.is_empty() {
        // Deepest level of the segment: entering would claim one more level than the
        // script asked for.
        return;
    }

    match guard.enter() {
        Ok(mut child) => {
            *depth = level + 1;
            drive(&mut child, rest, data, level + 1, limit, depth);
        }
        Err(e) => {
            overflow_assert(e, level + 1, limit);
        }
    }
}

/// The overflow error must name the level that was rejected, and the counter must not move.
fn overflow_assert(e: parsio::ReadError, expected: usize, limit: usize) {
    match e {
        parsio::ReadError::AllowedDepthOverflow { depth, limit: l } => {
            assert_eq!(
                (depth as usize, l as usize),
                (expected, limit),
                "overflow must report the rejected level (limit {limit})"
            );
            assert!(expected > limit, "overflow reported below limit {limit}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(target, input)` pairs embedded by `build.rs` — no filesystem access, so the replay runs
    /// under Miri's default isolation.
    mod embedded {
        include!(concat!(env!("OUT_DIR"), "/corpus_generated.rs"));
    }

    /// The observation ops need a script that contains them: the committed corpus predates them,
    /// so a constructed input pins the harness's model for `is_eof`/`remaining` here, on stable
    /// and under Miri with the rest of the suite.
    #[test]
    fn eof_observation_ops_match_the_model() {
        reset_allocated();
        fuzz_read_ops(&ReadInput {
            buf: b"abcdefgh".to_vec(),
            ops: vec![
                Op::IsEof,
                Op::Remaining,
                // Exactly to EOF from a fresh stream, so the bounded cells then sit in the state
                // only `Take` can be in: inner exhausted, budget left.
                Op::Advance(Length::Exact),
                Op::IsEof,
                Op::Remaining,
                Op::ReadSlice(Length::One),
                Op::IsEof,
                Op::Remaining,
                Op::ReadTillEof,
                Op::IsEof,
                Op::Remaining,
            ],
            capacity: 2,
            take_limit: 12,
        });
    }

    /// Replays the committed corpus through the same entry points the targets use. Runs on stable
    /// and under Miri, which libFuzzer cannot do.
    #[test]
    fn corpus_replay() {
        let mut seen = std::collections::BTreeMap::new();

        for (target, bytes) in embedded::CORPUS {
            *seen.entry(*target).or_insert(0usize) += 1;
            let mut u = arbitrary::Unstructured::new(bytes);

            match *target {
                "read_ops" => {
                    // Decode first, reset after — the live target (fuzz_targets/read_ops.rs)
                    // resets between decode and `fuzz_read_ops`, and the allocation budget
                    // must not count `Arbitrary` decoding here either.
                    if let Ok(input) = ReadInput::arbitrary(&mut u) {
                        reset_allocated();
                        fuzz_read_ops(&input);
                    }
                }
                "std_reader_adversarial" => {
                    if let Ok(input) = HostileInput::arbitrary(&mut u) {
                        fuzz_std_reader_adversarial(&input.read, &input.schedule);
                    }
                }
                "write_sinks" => {
                    if let Ok(input) = WriteInput::arbitrary(&mut u) {
                        fuzz_write_sinks(&input);
                    }
                }
                "depth_accounting" => {
                    if let Ok(input) = DepthInput::arbitrary(&mut u) {
                        fuzz_depth_accounting(&input);
                    }
                }
                other => panic!("unknown corpus target: {other}"),
            }
        }

        for target in [
            "read_ops",
            "std_reader_adversarial",
            "write_sinks",
            "depth_accounting",
        ] {
            assert!(
                seen.get(target).copied().unwrap_or(0) > 0,
                "corpus/{target} has no seeds to replay"
            );
        }
    }
}
