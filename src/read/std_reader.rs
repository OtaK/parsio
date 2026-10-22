use crate::{
    error::{ReadError, ReadResult},
    read::{Read, eof, find_needle, invalid_input, straddle_check},
};
use std::borrow::Cow;

/// Wrapper around any [`std::io::Read`] type, necessary not to conflict with the implementation on [`&[u8]`]
///
/// ## Warning
///
/// This is not zero-copy!
///
/// ### Caveats
///
/// On error, [`Read::advance`], [`Read::read_byte`], [`Read::read_slice`], [`Read::read_array`],
/// [`Read::read_till_eof`] and [`Read::read_until_match`] might already have consumed part of the
/// input, and those bytes are lost; a failing `read_slice` can leave the stream at EOF.
///
/// `read_until_match` and `read_till_eof` keep reading until the needle / EOF: wrap untrusted
/// streams in [`Take`](crate::Take) to bound their memory.
///
/// [`Read::read_slice`] trusts `len` up to 4MiB, past that it reserves at most 32x what actually arrived.
/// Need a hard cap? Use [`Take`](crate::Take).
///
/// ### Panics
///
/// On a `BufReader` with capacity 0: it can never buffer, so it would read as EOF or worse, block forever.
#[derive(Debug)]
#[repr(transparent)]
pub struct StdReader<R: std::io::Read>(std::io::BufReader<R>);

impl<R: std::io::Read> StdReader<R> {
    #[inline(always)]
    pub fn new(buf_reader: std::io::BufReader<R>) -> Self {
        Self::from(buf_reader)
    }

    #[inline(always)]
    pub fn into_inner(self) -> std::io::BufReader<R> {
        self.0
    }

    /// Much like std, we want to try to keep refilling if we get `Interrupted`
    #[inline]
    fn fill_buf_retry(&mut self) -> std::io::Result<&[u8]> {
        use std::io::BufRead as _;

        while let Err(e) = self.0.fill_buf() {
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(e);
            }
        }

        // Not `fill_buf()` again: on an empty buffer (EOF) that's another `read()`
        Ok(self.0.buffer())
    }

    #[inline]
    fn require_buffer_filled(&mut self, len: usize) -> Result<(), std::io::ErrorKind> {
        if self.0.buffer().len() >= len {
            return Ok(());
        }

        let buf = self.fill_buf_retry().map_err(|e| e.kind())?;

        if buf.len() >= len {
            Ok(())
        } else {
            Err(std::io::ErrorKind::UnexpectedEof)
        }
    }
}

impl<R: std::io::Read> From<std::io::BufReader<R>> for StdReader<R> {
    #[inline(always)]
    fn from(value: std::io::BufReader<R>) -> Self {
        assert!(
            value.capacity() > 0,
            "StdReader needs a BufReader with capacity > 0"
        );
        Self(value)
    }
}

impl<'data, T: std::io::Read> Read<'data> for StdReader<T> {
    #[inline]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        self.require_buffer_filled(1).map_err(ReadError::IoError)?;
        let b = unsafe { *self.0.buffer().as_ptr() };
        Ok(b)
    }

    /// Whatever is buffered in the internal BufReader. Basically a call to `BufReader::buffer()`
    #[inline]
    fn peek_slice(&self) -> &[u8] {
        self.0.buffer()
    }

    #[inline]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        use std::io::BufRead as _;
        let mut left_to_consume = n;
        while left_to_consume > 0 {
            let cur_buf_len = self.fill_buf_retry()?.len();
            // If the buffer is empty after filling it, consider EOF
            if cur_buf_len == 0 {
                return eof();
            }

            let amount_to_consume = left_to_consume.min(cur_buf_len);

            self.0.consume(amount_to_consume);
            left_to_consume = left_to_consume.saturating_sub(amount_to_consume);
        }
        Ok(())
    }

    #[inline]
    fn read_byte(&mut self) -> ReadResult<u8> {
        use std::io::Read as _;
        let mut b = 0u8;
        self.0.read_exact(std::slice::from_mut(&mut b))?;
        Ok(b)
    }

    fn read_slice<'a>(&'a mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        use std::io::Read as _;

        const CHUNKED_UP_TO: usize = 8 << 10; // 8KiB
        // Max we reserve on a length alone; each next step may reserve 32x of what actually arrived
        const RESERVE_FREE: usize = 4 << 20; // 4MiB
        const PROOF_SHIFT: u32 = 5;

        // Big reads go straight into the Vec, unchunked
        if len >= CHUNKED_UP_TO {
            let oom = |_| ReadError::IoError(std::io::ErrorKind::OutOfMemory);

            let mut cap = if len <= RESERVE_FREE {
                len
            } else {
                // cap at 32x the input at most, 4MiB min
                len.div_ceil(1 << PROOF_SHIFT).min(RESERVE_FREE)
            };

            let mut buf = Vec::new();
            loop {
                let want = cap - buf.len();
                buf.try_reserve_exact(want).map_err(oom)?;
                let got = self.0.by_ref().take(want as u64).read_to_end(&mut buf)?;

                if buf.len() == len {
                    return Ok(Cow::Owned(buf));
                }
                if got < want {
                    return eof();
                }
                cap = len.min(buf.len().saturating_mul(1 << PROOF_SHIFT));
            }
        }

        // Chunking time
        use std::io::BufRead as _;

        let mut buf: Vec<u8> = Vec::with_capacity(len);
        let mut left = len;

        while left > 0 {
            let filled = self.fill_buf_retry().map_err(ReadError::from)?;
            if filled.is_empty() {
                break;
            }

            let take = left.min(filled.len());
            let written = buf.len();

            // SAFETY: `buf` has `len` capacity and `written + take <= len`, so the destination is inside
            // its uninitialized spare capacity and never overlaps `filled`, which belongs to the buffered reader.
            // The length is set only after those bytes are initialized, like what `std` is doing
            unsafe {
                let dst = buf.spare_capacity_mut().as_mut_ptr().cast::<u8>();
                std::ptr::copy_nonoverlapping(filled.as_ptr(), dst, take);
                buf.set_len(written + take);
            }

            self.0.consume(take);
            left -= take;
        }

        if left != 0 {
            return eof();
        }

        Ok(Cow::Owned(buf))
    }

    #[inline]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        use std::io::Read as _;
        let mut arr = [0; N];
        self.0.read_exact(&mut arr)?;
        Ok(Cow::Owned(arr))
    }

    #[inline]
    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        use std::io::Read as _;
        let mut buf = vec![];
        self.0.read_to_end(&mut buf)?;
        Ok(Cow::Owned(buf))
    }

    #[inline]
    fn is_eof(&mut self) -> ReadResult<bool> {
        match self.require_buffer_filled(1).map_err(ReadError::IoError) {
            Ok(_) => Ok(false),
            Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof)) => Ok(true),
            Err(e) => Err(e),
        }
    }

    #[inline]
    fn remaining(&self) -> Option<usize> {
        None
    }

    #[inline]
    fn read_until_match(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        use std::io::BufRead as _;

        if needle.is_empty() {
            return invalid_input();
        }

        // Unfortunately can't prealloc since we don't know exactly
        // where/when we'll encounter the needle, if it exists at all.
        // The why is because Std readers might not have everything buffered AoT
        let mut out = Vec::new();
        let needle_len = needle.len();

        loop {
            let filled = self.fill_buf_retry().map_err(ReadError::from)?;
            if filled.is_empty() {
                return Ok((Cow::Owned(out), false));
            }

            // Check if our needle was actually sitting between our `out` and the newly
            // filled buffer. For multi-byte needles that can happen, so we have a fast path for it
            if let Some(k) = straddle_check(needle, needle_len, &out, filled) {
                out.truncate(out.len() - k);
                self.0.consume(needle_len - k);
                return Ok((Cow::Owned(out), true));
            }

            // Then scan the buffer itself, copying only the bytes the field actually takes rather
            // than the whole buffered chunk.
            match find_needle(filled, needle) {
                Some(index) => {
                    out.extend_from_slice(&filled[..index]);
                    self.0.consume(index + needle_len);
                    return Ok((Cow::Owned(out), true));
                }
                None => {
                    let taken = filled.len();
                    out.extend_from_slice(filled);
                    self.0.consume(taken);
                }
            }
        }
    }
}

// Blanket fwd impls for std::io stuff. Everything `BufReader` overrides is forwarded too, or the
// defaults would bypass its buffer.
impl<T: std::io::Read> std::io::Read for StdReader<T> {
    #[inline(always)]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }

    #[inline(always)]
    fn read_vectored(&mut self, bufs: &mut [std::io::IoSliceMut<'_>]) -> std::io::Result<usize> {
        self.0.read_vectored(bufs)
    }

    #[inline(always)]
    fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        self.0.read_exact(buf)
    }

    #[inline(always)]
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        self.0.read_to_end(buf)
    }

    #[inline(always)]
    fn read_to_string(&mut self, buf: &mut String) -> std::io::Result<usize> {
        self.0.read_to_string(buf)
    }
}

impl<T: std::io::Read> std::io::Seek for StdReader<T>
where
    T: std::io::Seek,
{
    #[inline(always)]
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }

    #[inline(always)]
    fn stream_position(&mut self) -> std::io::Result<u64> {
        self.0.stream_position()
    }

    #[inline(always)]
    fn seek_relative(&mut self, offset: i64) -> std::io::Result<()> {
        self.0.seek_relative(offset)
    }
}

// `StdReader` always holds a `BufReader`, so it's `BufRead` whatever the inner reader is
impl<T: std::io::Read> std::io::BufRead for StdReader<T> {
    #[inline(always)]
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.0.fill_buf()
    }

    #[inline(always)]
    fn consume(&mut self, amount: usize) {
        self.0.consume(amount)
    }
}
