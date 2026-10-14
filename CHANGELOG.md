# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## 0.1.0 - 2026-10-14

The time of maturity; bugfixes, new features, test/bench/fuzz infrastructure

The boring stuff was LLM-assisted, because I can't be bothered writing thousands of lines of test harnesses.

### Added

- Testing infrastructure (tests, proptesting, fuzzing, benches)
- `Read` blanket impls for `&mut R` and `Box<R>`, mirroring the existing `Write` impls.
- Blanket impl of `std::io::Read`, `std::io::Seek` and `std::io::BufRead` forwarding on `StdReader`.
- `Read::read_until(&[u8])`: reads up to the first occurrence of a needle and consumes it, returning the bytes before it plus whether it matched
  - This introduces a `simd` feature, enabled by default, which delegates to `memchr`/`memmem`
- `Read::peek_slice()`: Allows to look at the immediately available slice of data. It has fringe uses (mostly serves to enable some performance on the `Take` adapter)
- `Read::is_eof()` and `Read::remaining()`: ask whether the stream is at EOF and how many bytes are certainly left right now (`remaining` does no I/O and answers `None` when it cannot know, like `StdReader`)
- `Take` adapter to limit how many bytes are read
- `DepthAwareReader::depth()` / `limit()` and `DepthAwareReaderGuard::depth()` / `limit()`

### Fixed

- `StdReader::advance` now consumes exactly `n` bytes, refilling as needed, and returns `UnexpectedEof` past the end of the stream.
- `StdReader::read_slice` allocated `len` bytes before reading. Oversized lengths (`> 32MiB`) and small ones (`≤ 8KiB`) are now bounded by the data actually available; lengths in between still pre-reserve `len` up front on the unchunked fast path.
- `DepthAwareReader::enter`'s depth is only consumed when the level is actually entered.
- `&mut [u8]::write_all`'s length is now checked first and the buffer is left untouched when the input does not fit.
- `StdReader` now retries `Interrupted` on every buffered refill (`peek_byte`, `advance`, `read_slice`, `read_until`), matching the retry `read_byte`/`read_array`/`read_till_eof` present in `std`.

### Changed

- **Breaking:** `DepthAwareReaderGuard::enter` now borrows (`&mut self`) instead of consuming the guard (`mut self`), mirroring `DepthAwareReader::enter`. The returned guard accounts for a single level and reborrows the parent: chains are written by shadowing or recursion.
  - Previously, holding a parent guard across several children was a borrow error, so recursion (JSON arrays, XML elements, CBOR maps, etc) could only compile by dropping the guard, defeating the purpose of the whole thing
  - `guard = guard.enter()?` (assignment) no longer compiles. Use `let` shadowing now, it used to work, now it's rustc screaming at your face for being naughty.
- `StdReader::advance` past the end of the stream now returns `UnexpectedEof`.
- `&mut [u8]::write_all` no longer writes a partial prefix before failing.
- `StdWriter::write_vectored` preallocates the `IoSlice` buffer instead of growing it via `collect`.

## 0.0.2 - 2026-07-15

### Added

- `Read::read_till_eof`.
- `Read` implementations for `Vec<u8>` and `Cow<'data, [u8]>`.

## 0.0.1 - 2026-07-02

### Added

- Initial release: the `Read` and `Write` traits, `DepthAwareReader` (billion-laughs guard),
  `StdReader` / `StdWriter` adapters for `std::io`, and the `ReadError` / `WriteError` error
  types.
