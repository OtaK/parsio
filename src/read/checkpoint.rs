use std::borrow::Cow;

use crate::{Read, ReadResult};

/// Checkpointed reader that tracks the amount of bytes read
///
/// This is purposedly not buildable from the outside and does not expose
/// any constructor of any sort
pub struct Checkpoint<'a, 'data, R: Read<'data>> {
    pub(crate) inner: &'a mut R,
    pub(crate) amt_read: usize,
    pub(crate) _boo: std::marker::PhantomData<&'data ()>,
}

impl<'a, 'data, R: Read<'data>> std::fmt::Debug for Checkpoint<'a, 'data, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Checkpoint")
            .field("inner", &"[READER]")
            .field("amt_read", &self.amt_read)
            .finish()
    }
}

impl<'a, 'data, R: Read<'data>> Checkpoint<'a, 'data, R> {
    /// Returns the amount of bytes read since the beginning of the checkpoint
    #[inline]
    pub fn amt_read(&self) -> usize {
        self.amt_read
    }

    /// Releases the mutable borrow of the reader
    #[inline]
    pub fn into_inner(self) -> &'a mut R {
        self.inner
    }
}

impl<'a, 'data, R: Read<'data>> Read<'data> for Checkpoint<'a, 'data, R> {
    #[inline]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        self.inner.peek_byte()
    }

    #[inline]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        self.inner.advance(n)?;
        self.amt_read += n;
        Ok(())
    }

    #[inline]
    fn read_byte(&mut self) -> ReadResult<u8> {
        let b = self.inner.read_byte()?;
        self.amt_read += 1;
        Ok(b)
    }

    #[inline]
    fn read_slice<'b>(&'b mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        let slice = self.inner.read_slice(len)?;
        self.amt_read += len;
        Ok(slice)
    }

    #[inline]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        let array = self.inner.read_array()?;
        self.amt_read += N;
        Ok(array)
    }

    #[inline]
    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        let bytes = self.inner.read_till_eof()?;
        self.amt_read += bytes.len();
        Ok(bytes)
    }
}
