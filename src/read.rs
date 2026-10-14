mod adapters;
// implementations only, nothing to re-export
mod in_memory;
mod std_reader;

pub use self::{adapters::*, std_reader::*};

use std::borrow::Cow;

use crate::error::{ReadError, ReadResult};

/// Zero-copy-ish byte reader over a `'data`-lived buffer.
pub trait Read<'data> {
    /// Peek a single byte over the data stream, without advancing it.
    fn peek_byte(&mut self) -> ReadResult<u8>;
    /// The bytes that can be handed out right now, without consuming them and without doing I/O.
    ///
    /// This is a *partial* view, not the rest of the stream: it MAY be empty even though more data
    /// is coming, because a reader with no internal buffer has nothing to show. So an empty slice does
    /// NOT mean EOF.
    ///
    /// The default is empty, which is correct but slow as it causes byte-munching; implementors that buffer (like
    /// [`StdReader`](super::StdReader)) should override it.
    #[inline]
    fn peek_slice(&self) -> &[u8] {
        &[]
    }
    /// Manually advances the data stream by `n` bytes. Usually used in tandem with [`Self::peek_byte`]
    fn advance(&mut self, n: usize) -> ReadResult<()>;
    /// Read a single byte from the data stream
    fn read_byte(&mut self) -> ReadResult<u8>;
    /// Read a byte slice from the data stream of length `len`
    /// Errors out if `n` is bigger than the remaining data stream
    fn read_slice(&mut self, len: usize) -> ReadResult<Cow<'data, [u8]>>;
    /// Read a byte array from the data stream of len `N`
    /// Basically behaves the same as [`Self::read_slice`] but with a sized slice
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>>;
    /// Reads the data stream till EOF
    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>>;
    /// Allows to ask the underlying stream if there's anything left. Kinda like the `Fused` stream impl on async rust
    /// Returns `Ok(true)` when the stream is definitely EOF.
    fn is_eof(&mut self) -> ReadResult<bool>;
    /// How many bytes left in the stream, asked without any IO operations
    /// `None` means unknown (for many reasons, for example `StdReader` always answers `None` because there might be more coming)
    /// `Some(n)` means for sure we nave N bytes available *right now*
    fn remaining(&self) -> Option<usize>;
    /// Reads the data stream up to the first occurrence of `needle`, which is consumed along
    /// with everything before it.
    /// It returns (buf_without_needle, found). The needle is discarded, if found.
    fn read_until(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)>;
}

/// Locates `needle` in `haystack`, SIMD-accelerated when the `simd` feature is on.
#[inline]
pub(super) fn find_needle(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    std::cfg_select! {
        feature = "simd" => {
            if needle.len() == 1 {
                memchr::memchr(needle[0], haystack)
            } else {
                memchr::memmem::find(haystack, needle)
            }
        }
        _ => {
            if needle.len() == 1 {
                haystack.iter().position(|byte| *byte == needle[0])
            } else {
                haystack
                    .windows(needle.len())
                    .position(|window| window == needle)
            }
        }
    }
}

/// Check if `needle` is straddled between `buf` and `input`
#[inline]
fn straddle_check(needle: &[u8], needle_len: usize, buf: &[u8], input: &[u8]) -> Option<usize> {
    debug_assert_eq!(
        needle.len(),
        needle_len,
        "needle.len() != needle_len, impl error"
    );

    let tail = needle_len.saturating_sub(1).min(buf.len());

    (1..=tail).rev().find(|k| {
        buf.ends_with(&needle[..*k])
            && input.len() >= needle_len - k
            && input[..needle_len - k] == needle[*k..]
    })
}

pub trait LimitedReadExt<'data>: Read<'data> {
    fn take(&mut self, limit: usize) -> Take<'data, &mut Self> {
        Take::new(self, limit)
    }
}

pub trait DepthLimitedReadExt<'data>: Read<'data> {
    fn with_max_depth(&mut self, max_depth: usize) -> DepthAwareReader<'data, &mut Self> {
        DepthAwareReader::from_reader_with_limit(self, max_depth)
    }
}

#[inline]
pub(super) fn eof<T>() -> ReadResult<T> {
    Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof))
}

#[inline]
pub(super) fn invalid_input<T>() -> ReadResult<T> {
    Err(ReadError::IoError(std::io::ErrorKind::InvalidInput))
}

// Blanket impls
impl<'data, R: Read<'data> + ?Sized> Read<'data> for &mut R {
    #[inline(always)]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        (**self).peek_byte()
    }

    #[inline(always)]
    fn peek_slice(&self) -> &[u8] {
        (**self).peek_slice()
    }

    #[inline(always)]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        (**self).advance(n)
    }

    #[inline(always)]
    fn read_byte(&mut self) -> ReadResult<u8> {
        (**self).read_byte()
    }

    #[inline(always)]
    fn read_slice<'a>(&'a mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        (**self).read_slice(len)
    }

    #[inline(always)]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        (**self).read_array()
    }

    #[inline(always)]
    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        (**self).read_till_eof()
    }

    #[inline(always)]
    fn is_eof(&mut self) -> ReadResult<bool> {
        (**self).is_eof()
    }

    #[inline(always)]
    fn remaining(&self) -> Option<usize> {
        (**self).remaining()
    }

    #[inline(always)]
    fn read_until(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        (**self).read_until(needle)
    }
}

impl<'data, R: Read<'data> + ?Sized> Read<'data> for Box<R> {
    #[inline(always)]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        (**self).peek_byte()
    }

    #[inline(always)]
    fn peek_slice(&self) -> &[u8] {
        (**self).peek_slice()
    }

    #[inline(always)]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        (**self).advance(n)
    }

    #[inline(always)]
    fn read_byte(&mut self) -> ReadResult<u8> {
        (**self).read_byte()
    }

    #[inline(always)]
    fn read_slice<'a>(&'a mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        (**self).read_slice(len)
    }

    #[inline(always)]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        (**self).read_array()
    }

    #[inline(always)]
    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        (**self).read_till_eof()
    }

    #[inline(always)]
    fn is_eof(&mut self) -> ReadResult<bool> {
        (**self).is_eof()
    }

    #[inline(always)]
    fn remaining(&self) -> Option<usize> {
        (**self).remaining()
    }

    #[inline(always)]
    fn read_until(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        (**self).read_until(needle)
    }
}
