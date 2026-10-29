use crate::{
    error::{ReadError, ReadResult},
    read::{Read, eof, find_needle, invalid_input, straddle_check},
};
use std::borrow::Cow;

/// [`std::io::Take`]-style adapter, to limit the amount of bytes read; This
/// is very useful for TLV-like workflows (CBOR, TLS, etc)
#[derive(Debug)]
pub struct Take<R> {
    reader: R,
    bytes_remaining: usize,
}

impl<R> Take<R> {
    #[inline]
    pub fn new(reader: R, limit: usize) -> Self {
        Self {
            reader,
            bytes_remaining: limit,
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

    /// Resets the budget, like [`std::io::Take::set_limit`]
    #[inline]
    pub fn set_limit(&mut self, limit: usize) {
        self.bytes_remaining = limit;
    }

    #[inline]
    pub fn into_inner(self) -> R {
        self.reader
    }

    /// The inner reader may have eaten bytes before failing: drop the budget so a retry can't
    /// cross into the next frame. EOF is left alone, there's nothing past it.
    #[cold]
    fn failed(&mut self, e: ReadError) -> ReadError {
        if !matches!(e, ReadError::IoError(std::io::ErrorKind::UnexpectedEof)) {
            self.bytes_remaining = 0;
        }
        e
    }
}

/// What the window [`Read::peek_slice`] currently shows tells [`Take::read_until_match`] to do.
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

impl<'data, R: Read<'data>> Read<'data> for Take<R> {
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
        self.reader.advance(n).map_err(|e| self.failed(e))?;
        self.bytes_remaining -= n;
        Ok(())
    }

    #[inline]
    fn read_byte(&mut self) -> ReadResult<u8> {
        if self.is_exhausted() {
            return eof();
        }
        let b = self.reader.read_byte().map_err(|e| self.failed(e))?;
        self.bytes_remaining -= 1;
        Ok(b)
    }

    #[inline]
    fn read_slice(&mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        if len > self.bytes_remaining {
            return eof();
        }
        let bytes = self.reader.read_slice(len).map_err(|e| self.failed(e))?;
        self.bytes_remaining -= len;
        Ok(bytes)
    }

    #[inline]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        if N > self.bytes_remaining {
            return eof();
        }
        let bytes = self.reader.read_array().map_err(|e| self.failed(e))?;
        self.bytes_remaining -= N;
        Ok(bytes)
    }

    /// Whatever is left under the limit, fewer bytes if the inner stream ends first.
    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        // Known length: one read, borrowed over slices
        if let Some(n) = Read::remaining(self) {
            return self.read_slice(n);
        }

        let mut out = Vec::new();
        while self.bytes_remaining > 0 {
            let n = self.peek_slice().len();
            if n > 0 {
                out.extend_from_slice(&self.peek_slice()[..n]);
                self.advance(n)?;
                continue;
            }
            // Empty window: one byte refills it, or tells EOF
            match self.read_byte() {
                Ok(b) => out.push(b),
                Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof)) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(Cow::Owned(out))
    }

    /// EOF when the budget is spent *or* the inner stream is done.
    #[inline]
    fn is_eof(&mut self) -> ReadResult<bool> {
        Ok(self.bytes_remaining == 0 || self.reader.is_eof()?)
    }

    /// What the inner stream surely has, capped by the budget; `None` when the inner can't tell.
    #[inline]
    fn remaining(&self) -> Option<usize> {
        self.reader.remaining().map(|n| n.min(self.bytes_remaining))
    }

    /// Bulk scan over whichever slice the inner reader can show ([`Read::peek_slice`]), falling
    /// back to a byte-munching approach when there's nothing available.
    ///
    /// All of this is done purely for the sake of performance, so beware dragons yadda yadda
    // Not inlined because that's a lot of code
    fn read_until_match(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
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
                    // Nothing buffered yet: hand out the inner read as is (borrowed over slices)
                    if out.is_empty() {
                        let head = self.read_slice(field)?;
                        self.advance(needle_len)?; // Skip over needle
                        return Ok((head, true));
                    }
                    out.extend_from_slice(&self.peek_slice()[..field]);
                    self.advance(field + needle_len)?; // Field + needle
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
                    // Copy straight out of the window, not through an inner `read_slice`
                    out.extend_from_slice(&self.peek_slice()[..n]);
                    self.advance(n)?;
                }
            }
        }
    }
}

/// Hands out owned bytes only, so it reads as `Read<'any>` (`'static` included) over a borrowed
/// reader. For decoders that can't borrow their input buffer.
#[derive(Debug)]
pub struct Detached<R>(R);

impl<R> Detached<R> {
    #[inline]
    pub fn new(reader: R) -> Self {
        Self(reader)
    }

    #[inline]
    pub fn into_inner(self) -> R {
        self.0
    }
}

impl<'a, 'data, R: Read<'a>> Read<'data> for Detached<R> {
    #[inline(always)]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        self.0.peek_byte()
    }

    #[inline(always)]
    fn peek_slice(&self) -> &[u8] {
        self.0.peek_slice()
    }

    #[inline(always)]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        self.0.advance(n)
    }

    #[inline(always)]
    fn read_byte(&mut self) -> ReadResult<u8> {
        self.0.read_byte()
    }

    #[inline]
    fn read_slice(&mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        Ok(Cow::Owned(self.0.read_slice(len)?.into_owned()))
    }

    #[inline]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        Ok(Cow::Owned(self.0.read_array()?.into_owned()))
    }

    #[inline]
    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        Ok(Cow::Owned(self.0.read_till_eof()?.into_owned()))
    }

    #[inline(always)]
    fn is_eof(&mut self) -> ReadResult<bool> {
        self.0.is_eof()
    }

    #[inline(always)]
    fn remaining(&self) -> Option<usize> {
        self.0.remaining()
    }

    #[inline]
    fn read_until_match(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        let (bytes, found) = self.0.read_until_match(needle)?;
        Ok((Cow::Owned(bytes.into_owned()), found))
    }
}

/// Depth-aware wrapper around a [`Read`] implementation.
/// If you care about billion-laughs attacks, then you should use it.
#[derive(Debug)]
pub struct DepthAwareReader<R> {
    reader: R,
    limit: u16,
    depth: u16,
}

impl<R> DepthAwareReader<R> {
    /// Default depth limit
    pub const DEFAULT_LIMIT: u16 = 256;

    /// Initializes a DepthAwareReader using [`Self::DEFAULT_LIMIT`] as a depth limit
    #[inline(always)]
    pub fn from_reader(reader: R) -> Self {
        Self {
            reader,
            limit: Self::DEFAULT_LIMIT,
            depth: 0,
        }
    }

    /// Initializes a DepthAwareReader using a custom depth limit
    #[inline(always)]
    pub fn from_reader_with_limit(reader: R, limit: u16) -> Self {
        Self {
            reader,
            limit,
            depth: 0,
        }
    }

    #[inline]
    /// Copies current state (limit & current depth) to a new instance;
    /// This is used particularily for buffering and keeping depth integrity
    pub fn handoff_state<O>(&self, rdr: O) -> DepthAwareReader<O> {
        let mut dar = DepthAwareReader::from_reader_with_limit(rdr, self.limit);
        dar.depth = self.depth;
        dar
    }

    /// Increments the current depth, checks if it didn't exceed the limit and
    /// returns a Guard that exits this depth when dropped
    #[inline(always)]
    pub fn enter(&mut self) -> ReadResult<DepthAwareReaderGuard<'_, R>> {
        if self.depth >= self.limit {
            return Err(ReadError::AllowedDepthOverflow {
                depth: self.depth.saturating_add(1),
                limit: self.limit,
            });
        }
        self.depth += 1;
        Ok(DepthAwareReaderGuard::new(self))
    }

    /// The current depth level.
    #[inline]
    pub fn depth(&self) -> u16 {
        self.depth
    }

    /// The configured maximum depth level.
    #[inline]
    pub fn limit(&self) -> u16 {
        self.limit
    }

    #[inline]
    pub fn into_inner(self) -> R {
        self.reader
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
#[derive(Debug)]
pub struct DepthAwareReaderGuard<'a, R> {
    reader: &'a mut DepthAwareReader<R>,
}

impl<'a, R> DepthAwareReaderGuard<'a, R> {
    #[inline]
    fn new(rdr: &'a mut DepthAwareReader<R>) -> Self {
        Self { reader: rdr }
    }

    /// Enters one more depth level from this one, *without* giving this one up.
    ///
    /// The returned guard accounts for a single level: dropping it restores this one, which
    /// stays alive and keeps its own level. Dropping the mutable borrow allows to use the parent
    ///
    /// It's basically making good use of chained XOR borrows to modelize parser depth
    #[inline]
    pub fn enter(&mut self) -> ReadResult<DepthAwareReaderGuard<'_, R>> {
        if self.reader.depth >= self.reader.limit {
            return Err(ReadError::AllowedDepthOverflow {
                depth: self.reader.depth.saturating_add(1),
                limit: self.reader.limit,
            });
        }

        self.reader.depth += 1;
        Ok(DepthAwareReaderGuard::new(&mut *self.reader))
    }

    #[inline]
    /// Copies current state (limit & current depth) to a new instance;
    /// This is used particularily for buffering and keeping depth integrity
    pub fn handoff_state<O>(&self, rdr: O) -> DepthAwareReader<O> {
        self.reader.handoff_state(rdr)
    }

    /// Current depth level.
    #[inline]
    pub fn depth(&self) -> u16 {
        self.reader.depth
    }

    /// The configured maximum depth level.
    #[inline]
    pub fn limit(&self) -> u16 {
        self.reader.limit
    }
}

impl<'data, R: Read<'data>> Read<'data> for DepthAwareReaderGuard<'_, R> {
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
    fn read_until_match(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        self.reader.read_until_match(needle)
    }
}
impl<R> Drop for DepthAwareReaderGuard<'_, R> {
    #[inline(always)]
    fn drop(&mut self) {
        self.reader.depth -= 1;
    }
}

/// Delegate reader impl
impl<'data, R: Read<'data>> Read<'data> for DepthAwareReader<R> {
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
    fn read_until_match(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        self.reader.read_until_match(needle)
    }
}

#[cfg(test)]
mod tests {
    use crate::{DepthAwareReader, LimitedReadExt, Read, ReadError};

    #[test]
    fn smoke_depth_guard_can_be_limited() {
        let data: &[u8] = &[0, 1, 2, 3, 4, 5, 6];
        let mut reader = DepthAwareReader::from_reader(data);
        let mut guard = reader.enter().unwrap();
        let mut guard = guard.limited(1);
        // First read should work since the limit is 1 byte
        assert_eq!(guard.read_byte().unwrap(), 0);
        // Second read should return EOF
        assert!(matches!(
            guard.read_byte().unwrap_err(),
            ReadError::IoError(std::io::ErrorKind::UnexpectedEof)
        ));
    }
}
