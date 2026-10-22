# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## 0.1.0 - 2026-10-14

The time of maturity; bugfixes, new features, test/bench/fuzz infrastructure

The boring stuff was LLM-assisted, because I can't be bothered writing thousands of lines of test harnesses.

### Added

- Testing infrastructure (tests, proptesting, fuzzing, benches)
- `Read` blanket impls for `&mut R` and `Box<R>`, mirroring the existing `Write` impls.
- `Read::read_until_match(&[u8])`: reads up to the first occurrence of a needle and consumes it, returning the bytes before it plus whether it matched
  - This introduces a `simd` feature, enabled by default, which delegates to `memchr`/`memmem` (AVX2 included)
- `Read::peek_slice()`: Allows to look at the immediately available slice of data. It has fringe uses (mostly serves to enable some performance on the `Take` adapter)
- `Read::is_eof()` and `Read::remaining()`: ask whether the stream is at EOF and how many bytes are certainly left right now (`remaining` does no I/O and answers `None` when it cannot know, like `StdReader`)
- `Take` adapter to limit how many bytes are read; this is what bounds untrusted streams.
  - EOF when the budget is spent *or* the inner stream is done; `Read::remaining` is the inner's, capped by the budget.
  - A failed inner read (anything but EOF) drops the budget, so a retry on a non-blocking source can't read past the frame.
  - `into_inner` and `set_limit`.
- `LimitedReadExt::limited` and `DepthLimitedReadExt::with_max_depth`, implemented for every `Read` (so `r.limited(4).with_max_depth(2)` just works).
- `DepthAwareReader::depth()` / `limit()` / `into_inner()` / `handoff_state()`, and the same accessors on `DepthAwareReaderGuard`, which is now `Debug`.
- `Read` for `VecDeque<u8>`: owned and consumed in O(1) per read. `VecDeque::from(vec)` reuses the allocation, so it's the way to parse an owned buffer (e.g. `StdReader::read_slice`'s `Cow::Owned`); `Vec<u8>` and `Cow::Owned` shift what's left on every read (~355x slower on 1MiB of 20-byte chunks).

### Fixed

- `StdReader::advance` now consumes exactly `n` bytes, refilling as needed, and returns `UnexpectedEof` past the end of the stream.
- `StdReader::read_slice` allocated `len` bytes before reading. Now `len < 8KiB` is bounded by the data actually available, and anything bigger trusts `len` up to 4MiB, then reserves at most 32x what actually arrived. Reads go straight into the `Vec` and end with capacity exactly `len`.
- `StdReader::read_slice` returns `OutOfMemory` instead of aborting when the allocator says no.
- `StdReader` retries `Interrupted` on every buffered refill (`peek_byte`, `advance`, `read_slice`, `read_until_match`), matching the retry `read_byte`/`read_array`/`read_till_eof` get from `std`.
- `DepthAwareReader::enter` / `DepthAwareReaderGuard::enter` only count a level when it's actually entered, and can't overflow the counter.
- `&mut [u8]::write_all`'s length is now checked first and the buffer is left untouched when the input does not fit.

### Changed

- **Breaking:** `DepthAwareReaderGuard::enter` now borrows (`&mut self`) instead of consuming the guard (`mut self`), mirroring `DepthAwareReader::enter`. The returned guard accounts for a single level and reborrows the parent: chains are written by shadowing or recursion.
  - Previously, holding a parent guard across several children was a borrow error, so recursion (JSON arrays, XML elements, CBOR maps, etc) could only compile by dropping the guard, defeating the purpose of the whole thing
  - `guard = guard.enter()?` (assignment) no longer compiles. Use `let` shadowing now, it used to work, now it's rustc screaming at your face for being naughty.
- **Breaking:** `DepthAwareReader`'s limit is now a u16 - 65k max depth is plenty enough for pretty much *any* workflow.
  - The benefit is that now `ReadResult`/`ReadError` is much smaller on x64 platforms.
- **Breaking:** `DepthAwareReader` and `DepthAwareReaderGuard` lost their `'data` lifetime: `DepthAwareReader<R>`, `DepthAwareReaderGuard<'a, R>`.
- **Breaking:** `ReadError` and `WriteError` are `#[non_exhaustive]`.
- **Breaking:** `StdReader::new` / `From` panic on a `BufReader` with capacity 0, which can't buffer and would silently read as EOF.
- **Breaking:** the `alloc` feature is gone; `std` was already required.
- `StdReader::advance` past the end of the stream now returns `UnexpectedEof`.
- `StdReader` is `std::io::BufRead` for any inner reader, not only an inner `BufRead`, and its `std::io` forwards include everything `BufReader` specializes (`read_exact`, `read_to_end`, `stream_position`, ...), so going through `StdReader` keeps its buffer.
- `&mut [u8]::write_all` no longer writes a partial prefix before failing.
- `StdWriter::write_vectored` builds its `IoSlice`s on the stack instead of collecting a `Vec` per call. Only the first 64 buffers go out per call (partial writes were always allowed).

## 0.0.2 - 2026-07-15

### Added

- `Read::read_till_eof`.
- `Read` implementations for `Vec<u8>` and `Cow<'data, [u8]>`.

## 0.0.1 - 2026-07-02

### Added

- Initial release: the `Read` and `Write` traits, `DepthAwareReader` (billion-laughs guard),
  `StdReader` / `StdWriter` adapters for `std::io`, and the `ReadError` / `WriteError` error
  types.
