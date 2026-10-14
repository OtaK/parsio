use super::{DepthLimitedReadExt, LimitedReadExt, Read, eof, find_needle, invalid_input};

use std::borrow::Cow;

use crate::{
    error::{ReadError, ReadResult},
    read::straddle_check,
};

/// [`std::io::Take`]-style adapter, to limit the amount of bytes read; This
/// is very useful for TLV-like workflows (CBOR, TLS, etc)
#[derive(Debug)]
pub struct Take<'data, R: Read<'data>> {
    reader: R,
    bytes_remaining: usize,
    _boo: std::marker::PhantomData<&'data ()>,
}

impl<'data, R: Read<'data>> Take<'data, R> {
    #[inline]
    pub fn new(reader: R, limit: usize) -> Self {
        Self {
            reader,
            bytes_remaining: limit,
            _boo: Default::default(),
        }
    }

    #[inline]
    pub fn remaining(&self) -> usize {
        self.bytes_remaining
    }

    #[inline]
    pub fn is_exhausted(&self) -> bool {
        self.bytes_remaining == 0
    }
}

/// What the window [`Read::peek_slice`] currently shows tells [`Take::read_until`] to do.
enum ScanStep {
    /// `k` bytes of the needle are already at the end of `out`,
    /// the rest is at the head of the window.
    FoundStraddled(usize),
    /// We got a match of `field` bytes
    Found { field: usize },
    /// No match, consume `n` bytes and keep looking
    CopyNext(usize),
}

const SMALL_PREALLOC_AMT: usize = 64;

impl<'data, R: Read<'data>> Read<'data> for Take<'data, R> {
    #[inline]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        if self.is_exhausted() {
            return eof();
        }
        self.reader.peek_byte()
    }

    /// The inner window, clamped so the limit is never visible past its end.
    #[inline]
    fn peek_slice(&self) -> &[u8] {
        let window = self.reader.peek_slice();
        &window[..window.len().min(self.bytes_remaining)]
    }

    #[inline]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        if n > self.bytes_remaining {
            return eof();
        }
        self.reader.advance(n)?;
        self.bytes_remaining -= n;
        Ok(())
    }

    #[inline]
    fn read_byte(&mut self) -> ReadResult<u8> {
        if self.is_exhausted() {
            return eof();
        }
        let b = self.reader.read_byte()?;
        self.bytes_remaining -= 1;
        Ok(b)
    }

    #[inline]
    fn read_slice(&mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        if len > self.bytes_remaining {
            return eof();
        }
        let bytes = self.reader.read_slice(len)?;
        self.bytes_remaining -= len;
        Ok(bytes)
    }

    #[inline]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        if N > self.bytes_remaining {
            return eof();
        }
        let bytes = self.reader.read_array()?;
        self.bytes_remaining -= N;
        Ok(bytes)
    }

    #[inline]
    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        let ret = self.read_slice(self.bytes_remaining)?;
        self.bytes_remaining = 0;
        Ok(ret)
    }

    #[inline]
    fn is_eof(&mut self) -> ReadResult<bool> {
        Ok(self.bytes_remaining == 0)
    }

    #[inline]
    fn remaining(&self) -> Option<usize> {
        Some(self.bytes_remaining)
    }

    /// Bulk scan over whichever slice the inner reader can show ([`Read::peek_slice`]), falling
    /// back to a byte-munching approach when there's nothing available.
    ///
    /// All of this is done purely for the sake of performance, so beware dragons yadda yadda
    // Not inlined because that's a lot of code
    fn read_until(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        if needle.is_empty() {
            return invalid_input();
        }

        let needle_len = needle.len();
        let mut out = Vec::new();

        loop {
            // Scan step
            let step = {
                let next_slice = self.peek_slice();

                if next_slice.is_empty() {
                    // Keep going
                    ScanStep::CopyNext(0)
                } else {
                    // Pre-check if the match is already available, but straddled across our
                    // `out` buffer and the incoming slice
                    straddle_check(needle, needle_len, &out, next_slice)
                        .map(ScanStep::FoundStraddled)
                        // Not straddled, use our usual scan-ahead (memchr)
                        .unwrap_or_else(|| match find_needle(next_slice, needle) {
                            Some(field) => ScanStep::Found { field },
                            None => ScanStep::CopyNext(next_slice.len()),
                        })
                }
            };

            match step {
                ScanStep::FoundStraddled(k) => {
                    out.truncate(out.len() - k);
                    self.advance(needle_len - k)?; // Skip over needle
                    return Ok((Cow::Owned(out), true));
                }
                ScanStep::Found { field } => {
                    let head = self.read_slice(field)?;
                    out.extend_from_slice(&head);
                    self.advance(needle_len)?; // Skip over needle
                    return Ok((Cow::Owned(out), true));
                }
                ScanStep::CopyNext(0) => {
                    // Nothing matched before this, so preallocate a bit of memory
                    // since we're kinda sure we're going to move ahead, byte by byte
                    if out.capacity() == 0 {
                        out.reserve(SMALL_PREALLOC_AMT);
                    }

                    // A drained StdReader usually refills on the next read,
                    // and the scan above gets another slice run
                    loop {
                        match self.read_byte() {
                            Ok(byte) => {
                                out.push(byte);
                                if out.ends_with(needle) {
                                    out.truncate(out.len() - needle_len);
                                    return Ok((Cow::Owned(out), true));
                                }
                            }
                            Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof)) => {
                                return Ok((Cow::Owned(out), false));
                            }
                            Err(error) => return Err(error),
                        }

                        if !self.peek_slice().is_empty() {
                            break;
                        }
                    }
                }
                ScanStep::CopyNext(n) => {
                    let chunk = self.read_slice(n)?;
                    out.extend_from_slice(&chunk);
                }
            }
        }
    }
}

impl<'data, R> DepthLimitedReadExt<'data> for Take<'data, R> where
    R: Read<'data> + DepthLimitedReadExt<'data>
{
}

/// Depth-aware wrapper around a [`Read`] implementation.
/// If you care about billion-laughs attacks, then you should use it.
#[derive(Debug)]
pub struct DepthAwareReader<'data, R: Read<'data>> {
    reader: R,
    limit: usize,
    depth: usize,
    _marker: std::marker::PhantomData<&'data ()>,
}

impl<'data, R: Read<'data>> DepthAwareReader<'data, R> {
    /// Default depth limit
    pub const DEFAULT_LIMIT: usize = 256;

    /// Initializes a DepthAwareReader using [`Self::DEFAULT_LIMIT`] as a depth limit
    #[inline(always)]
    pub fn from_reader(reader: R) -> Self {
        Self {
            reader,
            limit: Self::DEFAULT_LIMIT,
            depth: 0,
            _marker: Default::default(),
        }
    }

    /// Initializes a DepthAwareReader using a custom depth limit
    #[inline(always)]
    pub fn from_reader_with_limit(reader: R, limit: usize) -> Self {
        Self {
            reader,
            limit,
            depth: 0,
            _marker: Default::default(),
        }
    }

    /// Increments the current depth, checks if it didn't exceed the limit and
    /// returns a Guard that exits this depth when dropped
    #[inline(always)]
    pub fn enter(&mut self) -> ReadResult<DepthAwareReaderGuard<'_, 'data, R>> {
        let next_depth = self.depth + 1;
        if next_depth <= self.limit {
            self.depth = next_depth;
            Ok(DepthAwareReaderGuard::new(self))
        } else {
            Err(ReadError::AllowedDepthOverflow {
                depth: next_depth,
                limit: self.limit,
            })
        }
    }

    /// The current depth level.
    #[inline]
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// The configured maximum depth level.
    #[inline]
    pub fn limit(&self) -> usize {
        self.limit
    }
}

/// Guard that decrements the current depth when dropped.
///
/// While a guard is live, the reader it came from is exclusively borrowed:
/// use the guard to consume the stream at this level. (implements `Read`)
///
/// Composition rules:
/// - A chain is written by shadowing: `let mut guard = guard.enter()?;`. The shadowed guard is
///   not dropped immediately as bindings live until the end of their scope and drop in
///   reverse declaration order. Assigning instead (`guard = guard.enter()?`) does not compile due
///   to aliasing rules
/// - A level with several children re-enters from the guard: the child is scoped to the child's
///   parse (`let mut child = guard.enter()?;` inside the loop), so the parent is readable again
///   once the last child drops.
pub struct DepthAwareReaderGuard<'a, 'data, R: Read<'data>> {
    reader: &'a mut DepthAwareReader<'data, R>,
}

impl<'a, 'data, R: Read<'data>> DepthAwareReaderGuard<'a, 'data, R> {
    #[inline]
    fn new(rdr: &'a mut DepthAwareReader<'data, R>) -> Self {
        Self { reader: rdr }
    }

    /// Enters one more depth level from this one, *without* giving this one up.
    ///
    /// The returned guard accounts for a single level: dropping it restores this one, which
    /// stays alive and keeps its own level. Dropping the mutable borrow allows to use the parent
    ///
    /// It's basically making good use of chained XOR borrows to modelize parser depth
    #[inline]
    pub fn enter(&mut self) -> ReadResult<DepthAwareReaderGuard<'_, 'data, R>> {
        let next_depth = self.reader.depth + 1;
        if next_depth > self.reader.limit {
            return Err(ReadError::AllowedDepthOverflow {
                depth: next_depth,
                limit: self.reader.limit,
            });
        }

        self.reader.depth = next_depth;
        Ok(DepthAwareReaderGuard::new(&mut *self.reader))
    }

    /// Current depth level.
    #[inline]
    pub fn depth(&self) -> usize {
        self.reader.depth
    }

    /// The configured maximum depth level.
    #[inline]
    pub fn limit(&self) -> usize {
        self.reader.limit
    }
}

impl<'a, 'data, R: Read<'data>> Read<'data> for DepthAwareReaderGuard<'a, 'data, R> {
    #[inline(always)]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        self.reader.peek_byte()
    }

    #[inline(always)]
    fn peek_slice(&self) -> &[u8] {
        self.reader.peek_slice()
    }

    #[inline(always)]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        self.reader.advance(n)
    }

    #[inline(always)]
    fn read_byte(&mut self) -> ReadResult<u8> {
        self.reader.read_byte()
    }

    #[inline(always)]
    fn read_slice(&mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        self.reader.read_slice(len)
    }

    #[inline(always)]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        self.reader.read_array()
    }

    #[inline(always)]
    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        self.reader.read_till_eof()
    }

    #[inline]
    fn is_eof(&mut self) -> ReadResult<bool> {
        self.reader.is_eof()
    }

    #[inline]
    fn remaining(&self) -> Option<usize> {
        self.reader.remaining()
    }

    #[inline(always)]
    fn read_until(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        self.reader.read_until(needle)
    }
}
impl<'a, 'data, R: Read<'data>> Drop for DepthAwareReaderGuard<'a, 'data, R> {
    #[inline(always)]
    fn drop(&mut self) {
        self.reader.depth -= 1;
    }
}

/// Delegate reader impl
impl<'data, R: Read<'data>> Read<'data> for DepthAwareReader<'data, R> {
    #[inline(always)]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        self.reader.peek_byte()
    }

    #[inline(always)]
    fn peek_slice(&self) -> &[u8] {
        self.reader.peek_slice()
    }

    #[inline(always)]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        self.reader.advance(n)
    }

    #[inline(always)]
    fn read_byte(&mut self) -> ReadResult<u8> {
        self.reader.read_byte()
    }

    #[inline(always)]
    fn read_slice<'a>(&'a mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        self.reader.read_slice(len)
    }

    #[inline(always)]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        self.reader.read_array()
    }

    #[inline(always)]
    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        self.reader.read_till_eof()
    }

    #[inline]
    fn is_eof(&mut self) -> ReadResult<bool> {
        self.reader.is_eof()
    }

    #[inline]
    fn remaining(&self) -> Option<usize> {
        self.reader.remaining()
    }

    #[inline(always)]
    fn read_until(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        self.reader.read_until(needle)
    }
}

impl<'data, R> LimitedReadExt<'data> for DepthAwareReader<'data, R> where
    R: Read<'data> + LimitedReadExt<'data>
{
}
