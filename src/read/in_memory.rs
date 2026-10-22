use crate::{
    error::ReadResult,
    read::{Read, eof, find_needle, invalid_input},
};

use std::borrow::Cow;

/// Implementation on base slices. This is what you'd want to use when having something that can hold
/// in-memory or when memory-mapping large-ish files
///
/// ## Attention
/// - Careful, Windows isn't very happy with 4GB memory-maps because 32-bit mmap is so 1980.
impl<'data> Read<'data> for &'data [u8] {
    #[inline]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        if self.is_empty() {
            return eof();
        }
        // SAFETY: The precondition above satisfies invariants
        Ok(unsafe { *self.as_ptr() })
    }

    #[inline]
    fn peek_slice(&self) -> &[u8] {
        self
    }

    #[inline]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        if n > self.len() {
            return eof();
        }

        // SAFETY: The check above fulfill the invariants required by this function
        *self = unsafe { std::slice::from_raw_parts(self.as_ptr().add(n), self.len() - n) };
        Ok(())
    }

    #[inline]
    fn read_byte(&mut self) -> ReadResult<u8> {
        if self.is_empty() {
            return eof();
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
            return eof();
        };

        *self = end;
        Ok(Cow::Borrowed(start))
    }

    #[inline]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        let Some((start, end)) = self.split_at_checked(N) else {
            return eof();
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

    #[inline]
    fn is_eof(&mut self) -> ReadResult<bool> {
        Ok(self.is_empty())
    }

    #[inline]
    fn remaining(&self) -> Option<usize> {
        Some(self.len())
    }

    #[inline]
    fn read_until_match(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        if needle.is_empty() {
            return invalid_input();
        }

        match find_needle(self, needle) {
            Some(index) => {
                let head = &self[..index];
                *self = &self[index + needle.len()..];
                Ok((Cow::Borrowed(head), true))
            }
            None => {
                let rest = *self;
                *self = &[];
                Ok((Cow::Borrowed(rest), false))
            }
        }
    }
}

#[inline]
fn is_head_heavy(len: usize, to_be_consumed: usize) -> bool {
    debug_assert!(
        to_be_consumed <= len,
        "cannot take {to_be_consumed} bytes out of a {len} byte buffer"
    );
    to_be_consumed > len.saturating_sub(to_be_consumed)
}

/// Removes and returns the first `to_be_consumed` bytes of `buf`, copying whichever half is smaller.
#[inline]
fn take_prefix(buf: &mut Vec<u8>, to_be_consumed: usize) -> Vec<u8> {
    if is_head_heavy(buf.len(), to_be_consumed) {
        let tail = buf.split_off(to_be_consumed);
        std::mem::replace(buf, tail)
    } else {
        let taken = buf[..to_be_consumed].to_vec();
        buf.drain(..to_be_consumed);
        taken
    }
}

/// Slow: every consuming read shifts what's left, O(remaining) each.
/// Borrow it as `&[u8]`, or read it as `VecDeque::from(vec)` to own and consume it in O(1).
impl<'data> Read<'data> for Vec<u8> {
    #[inline(always)]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        self.as_slice().peek_byte()
    }

    #[inline(always)]
    fn peek_slice(&self) -> &[u8] {
        self.as_slice()
    }

    #[inline]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        if n > self.len() {
            return eof();
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
            return eof();
        }
        Ok(Cow::Owned(take_prefix(self, len)))
    }

    #[inline]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        if N > self.len() {
            return eof();
        }

        let mut array = [0u8; N];
        array.copy_from_slice(&self[..N]);
        self.drain(..N);
        Ok(Cow::Owned(array))
    }

    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        Ok(Cow::Owned(std::mem::take(self)))
    }

    #[inline]
    fn is_eof(&mut self) -> ReadResult<bool> {
        Ok(self.is_empty())
    }

    #[inline]
    fn remaining(&self) -> Option<usize> {
        Some(self.len())
    }

    fn read_until_match(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        if needle.is_empty() {
            return invalid_input();
        }

        match find_needle(self.as_slice(), needle) {
            Some(index) => {
                let mut field = take_prefix(self, index + needle.len());
                field.truncate(index);
                Ok((Cow::Owned(field), true))
            }
            None => Ok((Cow::Owned(std::mem::take(self)), false)),
        }
    }
}

/// `Cow::Owned` reads like `Vec<u8>`: O(remaining) per consuming read, see there.
impl<'data> Read<'data> for Cow<'data, [u8]> {
    #[inline]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        match self {
            Cow::Borrowed(slice) => slice.peek_byte(),
            Cow::Owned(vec) => vec.as_slice().peek_byte(),
        }
    }

    #[inline]
    fn peek_slice(&self) -> &[u8] {
        match self {
            Cow::Borrowed(slice) => slice,
            Cow::Owned(vec) => vec.as_slice(),
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

    #[inline]
    fn is_eof(&mut self) -> ReadResult<bool> {
        Ok(self.is_empty())
    }

    #[inline]
    fn remaining(&self) -> Option<usize> {
        Some(self.len())
    }

    #[inline]
    fn read_until_match(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        match self {
            Cow::Borrowed(slice) => slice.read_until_match(needle),
            Cow::Owned(vec) => vec.read_until_match(needle),
        }
    }
}

/// `VecDeque::from(vec)` reuses the allocation; reads just move the head.
impl<'data> Read<'data> for std::collections::VecDeque<u8> {
    #[inline]
    fn peek_byte(&mut self) -> ReadResult<u8> {
        match self.front() {
            Some(&b) => Ok(b),
            None => eof(),
        }
    }

    #[inline]
    fn peek_slice(&self) -> &[u8] {
        self.as_slices().0
    }

    #[inline]
    fn advance(&mut self, n: usize) -> ReadResult<()> {
        if n > self.len() {
            return eof();
        }
        self.drain(..n);
        Ok(())
    }

    #[inline]
    fn read_byte(&mut self) -> ReadResult<u8> {
        match self.pop_front() {
            Some(b) => Ok(b),
            None => eof(),
        }
    }

    #[inline]
    fn read_slice(&mut self, len: usize) -> ReadResult<Cow<'data, [u8]>> {
        if len > self.len() {
            return eof();
        }
        let out = self.make_contiguous()[..len].to_vec();
        self.drain(..len);
        Ok(Cow::Owned(out))
    }

    #[inline]
    fn read_array<const N: usize>(&mut self) -> ReadResult<Cow<'data, [u8; N]>> {
        let Some(&arr) = self.make_contiguous().first_chunk::<N>() else {
            return eof();
        };
        self.drain(..N);
        Ok(Cow::Owned(arr))
    }

    fn read_till_eof(&mut self) -> ReadResult<Cow<'data, [u8]>> {
        Ok(Cow::Owned(Vec::from(std::mem::take(self))))
    }

    #[inline]
    fn is_eof(&mut self) -> ReadResult<bool> {
        Ok(self.is_empty())
    }

    #[inline]
    fn remaining(&self) -> Option<usize> {
        Some(self.len())
    }

    fn read_until_match(&mut self, needle: &[u8]) -> ReadResult<(Cow<'data, [u8]>, bool)> {
        if needle.is_empty() {
            return invalid_input();
        }
        // O(1) unless something wrapped it
        let window = self.make_contiguous();
        match find_needle(window, needle) {
            Some(index) => {
                let field = window[..index].to_vec();
                self.drain(..index + needle.len());
                Ok((Cow::Owned(field), true))
            }
            None => Ok((Cow::Owned(Vec::from(std::mem::take(self))), false)),
        }
    }
}
