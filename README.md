# parsio

[![Crates.io](https://img.shields.io/crates/v/parsio.svg)](https://crates.io/crates/parsio)
[![docs.rs](https://docs.rs/parsio/badge.svg)](https://docs.rs/parsio)

## Description

Parser-related utilities.
Mostly a `Read` and a `Write` implementation, suitable for zero-copy environments.

This makes use of quite a bit of unsafe for speed purposes.

See the docs to see if it fits you.

As an example on how to use this crate, you can check out [seabored](https://crates.io/crates/seabored).
As a matter of fact, this crate contains & extracts the I/O traits developed for `seabored`.

## Examples

The `examples/` folder contains toy CSV & TLV parsers to show how stuff can work.

```console
cargo run --example tlv
cargo run --example csv
```

## AI Disclaimer

Unlike a lot of things being created currently, the code of this library was written WITHOUT the use of any LLM. Anything under /src was 100% hand-written.

The test infrastructure, on the other hand, was LLM-assisted, because I can't be bothered. Same goes for the examples, which were completely LLM-gen'd.

## License

Licensed under either of these:

- Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
  [https://www.apache.org/licenses/LICENSE-2.0](https://www.apache.org/licenses/LICENSE-2.0))
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  [https://opensource.org/licenses/MIT](https://opensource.org/licenses/MIT))
