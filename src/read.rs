use std::borrow::Cow;

use crate::{
    checkpoint::Checkpoint,
    error::{ReadError, ReadResult},
};

pub mod checkpoint;

pub trait Read<'data> {
    /// Peek a single byte over the data stream, without advancing it.
    fn peek_byte(&mut self) -> ReadResult<u8>;
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

    /// Starts a checkpointed read, which allows you to track how many bytes
    /// total were read during that time
    ///
    /// This is a fringe use for certain data formats that communicate
    /// the byte length instead of the number of items for item collections (eg: TLSPL)
    #[inline]
    fn checkpoint<'a>(&'a mut self) -> Checkpoint<'a, 'data, Self>
    where
        Self: Sized,
    {
        Checkpoint {
            inner: self,
            amt_read: 0,
            _boo: Default::default(),
        }
    }
}

#[derive(Debug)]
/// Depth-aware wrapper around a [`Read`] implementation.
/// If you care about billion-laughs attacks, then you should use it.
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
        self.depth += 1;
        if self.depth <= self.limit {
            Ok(DepthAwareReaderGuard::new(self))
        } else {
            Err(ReadError::AllowedDepthOverflow {
                depth: self.depth,
                limit: self.limit,
            })
        }
    }
}

/// Guard that decredements the current depth when dropped
pub struct DepthAwareReaderGuard<'a, 'data, R: Read<'data>> {
    reader: &'a mut DepthAwareReader<'data, R>,
    accumulated_depth: usize,
}

impl<'a, 'data, R: Read<'data>> DepthAwareReaderGuard<'a, 'data, R> {
    #[inline]
    fn new(rdr: &'a mut DepthAwareReader<'data, R>) -> Self {
        Self {
            reader: rdr,
            accumulated_depth: 1,
        }
    }

    #[inline]
    pub fn enter(mut self) -> ReadResult<Self> {
        self.reader.depth += 1;
        self.accumulated_depth += 1;
        if self.reader.depth <= self.reader.limit {
            Ok(self)
        } else {
            Err(ReadError::AllowedDepthOverflow {
                depth: self.reader.depth,
                limit: self.reader.limit,
            })
        }
    }
}

impl<'a, 'data, R: Read<'data>> Drop for DepthAwareReaderGuard<'a, 'data, R> {
    #[inline(always)]
    fn drop(&mut self) {
        self.reader.depth -= self.accumulated_depth;
    }
}

/// Delegate reader impl
impl<'data, R: Read<'data>> Read<'data> for DepthAwareReader<'data, R> {
    #[inline(always)]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        self.reader.peek_byte()
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
}

/// Implementation on base slices. This is what you'd want to use when having something that can hold
/// in-memory or when memory-mapping large-ish files
///
/// ## Attention
/// - Careful, Windows isn't very happy with 4GB memory-maps because 32-bit mmap is so 1980.
impl<'data> Read<'data> for &'data [u8] {
    #[inline]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        if self.is_empty() {
            return Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof));
        }
        // SAFETY: The precondition above satisfies invariants
        Ok(unsafe { *self.as_ptr() })
    }

    #[inline]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        if n > self.len() {
            return Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof));
        }

        // SAFETY: The check above fulfill the invariants required by this function
        *self = unsafe { std::slice::from_raw_parts(self.as_ptr().add(n), self.len() - n) };
        Ok(())
    }

    #[inline]
    fn read_byte(&mut self) -> ReadResult<u8> {
        if self.is_empty() {
            return Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof));
        }
        let ptr = self.as_ptr();
        // SAFETY: The precondition above satisfies invariants of dereferencing the buffer pointer
        // (it dereferences to the first element of the slice, which is a byte)
        let b = unsafe { *ptr };
        // SAFETY: The above call will error out if the preconditions for from_raw_parts wouldn't be fulfilled
        *self = unsafe { std::slice::from_raw_parts(ptr.add(1), self.len() - 1) };
        Ok(b)
    }

    #[inline]
    fn read_slice<'a>(&'a mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        let Some((start, end)) = self.split_at_checked(len) else {
            return Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof));
        };

        *self = end;
        Ok(Cow::Borrowed(start))
    }

    #[inline]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        let Some((start, end)) = self.split_at_checked(N) else {
            return Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof));
        };

        *self = end;
        // SAFETY: The `start` slice will always be exactly N elements long, so this is safe.
        // Anyway this is basically what `slice::as_array` is, but without the length checks
        // as they are done above in `split_at_checked`.
        //
        // From std:
        // > SAFETY: The underlying array of a slice can be reinterpreted as an actual
        // > array `[T; N]` if `N` is not greater than the slice's length.
        Ok(Cow::Borrowed(unsafe { &*start.as_ptr().cast() }))
    }

    #[inline]
    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        self.read_slice(self.len())
    }
}

impl<'data> Read<'data> for Vec<u8> {
    #[inline(always)]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        self.as_slice().peek_byte()
    }

    #[inline]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        if n > self.len() {
            return Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof));
        }
        self.drain(..n);
        Ok(())
    }

    #[inline]
    fn read_byte(&mut self) -> ReadResult<u8> {
        let b = self.peek_byte()?;
        self.advance(1)?;
        Ok(b)
    }

    #[inline]
    fn read_slice(&mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        if len > self.len() {
            return Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof));
        }
        let end = self.split_off(len);
        Ok(Cow::Owned(std::mem::replace(self, end)))
    }

    #[inline]
    /// Careful, this double copies, hurting the performance quite a bit. Prefer using a StdReader for this
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        if N > self.len() {
            return Err(ReadError::IoError(std::io::ErrorKind::UnexpectedEof));
        }

        let end = self.split_off(N);
        let target = std::mem::replace(self, end);
        let mut array = [0u8; N];
        array.copy_from_slice(&target);
        Ok(Cow::Owned(array))
    }

    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        Ok(Cow::Owned(std::mem::take(self)))
    }
}

impl<'data> Read<'data> for Cow<'data, [u8]> {
    #[inline]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        match self {
            Cow::Borrowed(slice) => slice.peek_byte(),
            Cow::Owned(vec) => vec.as_slice().peek_byte(),
        }
    }

    #[inline]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        match self {
            Cow::Borrowed(slice) => slice.advance(n),
            Cow::Owned(vec) => vec.advance(n),
        }
    }

    #[inline]
    fn read_byte(&mut self) -> ReadResult<u8> {
        match self {
            Cow::Borrowed(slice) => slice.read_byte(),
            Cow::Owned(vec) => vec.read_byte(),
        }
    }

    #[inline]
    fn read_slice<'a>(&'a mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        match self {
            Cow::Borrowed(slice) => slice.read_slice(len),
            Cow::Owned(vec) => vec.read_slice(len),
        }
    }

    #[inline]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        match self {
            Cow::Borrowed(slice) => slice.read_array(),
            Cow::Owned(vec) => vec.read_array(),
        }
    }

    #[inline]
    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        match self {
            Cow::Borrowed(slice) => slice.read_till_eof(),
            Cow::Owned(vec) => vec.read_till_eof(),
        }
    }
}

#[derive(Debug)]
#[repr(transparent)]
/// Wrapper around any [`std::io::Read`] type, necessary not to conflict with the implementation on [`&[u8]`]
///
/// ## Warning
///
/// This is not zero-copy!
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

    #[inline]
    fn require_buffer_filled(&mut self, len: usize) -> Result<(), std::io::ErrorKind> {
        if self.0.buffer().len() >= len {
            return Ok(());
        }
        use std::io::BufRead as _;

        let buf = self.0.fill_buf().map_err(|e| e.kind())?;

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

    #[inline]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        use std::io::BufRead as _;
        self.0.consume(n);
        Ok(())
    }

    #[inline]
    fn read_byte(&mut self) -> ReadResult<u8> {
        use std::io::Read as _;
        let mut b = 0u8;
        self.0.read_exact(std::slice::from_mut(&mut b))?;
        Ok(b)
    }

    #[inline]
    fn read_slice<'a>(&'a mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        use std::io::Read as _;
        let mut buf = vec![0; len];
        self.0.read_exact(&mut buf)?;
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
}

// Blanket fwd impls for std::io stuff
impl<T: std::io::Read> std::io::Read for StdReader<T> {
    #[inline(always)]
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
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
}

impl<T: std::io::BufRead> std::io::BufRead for StdReader<T> {
    #[inline(always)]
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.0.fill_buf()
    }

    #[inline(always)]
    fn consume(&mut self, amount: usize) {
        self.0.consume(amount)
    }
}
