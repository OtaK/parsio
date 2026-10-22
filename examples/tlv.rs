//! Zero-copy length-prefixed frames.
//!
//! Layout, per frame: `kind: u8`, `len: u32` little-endian, `payload: [u8; len]`.
//! Run with `cargo run --example tlv`.
//!
//! The point of the example is the `Cow`: the very same `read_frames` function borrows payloads
//! straight out of the input when reading a slice (no allocation, no copy), and hands out owned
//! buffers when reading from a `std::io::Read` such as a `File`.

use std::borrow::Cow;
use std::io::{BufReader, ErrorKind};

use parsio::{Read, ReadError, ReadResult, StdReader};

const KIND_TEXT: u8 = 1;
const KIND_BLOB: u8 = 2;

/// Builds the input, so the example needs no fixture files.
fn encode() -> Vec<u8> {
    let mut out = Vec::new();
    for (kind, payload) in [
        (KIND_TEXT, b"hello".as_slice()),
        (KIND_BLOB, &[0xde, 0xad, 0xbe, 0xef]),
        (KIND_TEXT, b"last frame".as_slice()),
    ] {
        out.push(kind);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
    }
    out
}

/// Reads every frame until the stream ends.
///
/// `R: Read<'d>` is the whole API surface: this compiles for `&[u8]`, `Vec<u8>`, `Cow`,
/// `StdReader<_>` and `DepthAwareReader<_>` alike.
fn read_frames<'d, R: Read<'d>>(mut reader: R) -> ReadResult<Vec<(u8, Cow<'d, [u8]>)>> {
    let mut frames = Vec::new();

    loop {
        let kind = match reader.read_byte() {
            Ok(kind) => kind,
            // A clean end of stream is only legal between frames.
            Err(ReadError::IoError(ErrorKind::UnexpectedEof)) => return Ok(frames),
            Err(error) => return Err(error),
        };

        let len = u32::from_le_bytes(*reader.read_array::<4>()?);
        let payload = reader.read_slice(len as usize)?;
        frames.push((kind, payload));
    }
}

fn main() {
    let data = encode();
    println!("input: {} bytes\n", data.len());

    // 1. In memory: every payload is a borrow into `data`.
    let frames = read_frames(&data[..]).expect("in-memory parse cannot fail");
    report("&[u8]", &frames);

    // 2. Any `std::io::Read` — a `File` would look exactly the same: same code, owned payloads.
    let frames = read_frames(StdReader::new(BufReader::new(&data[..]))).expect("stream parse");
    report("StdReader", &frames);

    // 3. Truncated input: a short frame is an error, never a silent partial payload.
    let truncated = &data[..data.len() - 2];
    match read_frames(truncated) {
        Ok(frames) => println!(
            "\ntruncated: parsed {} frames, which should not happen",
            frames.len()
        ),
        Err(error) => println!("\ntruncated -> {error}"),
    }
}

fn report(source: &str, frames: &[(u8, Cow<'_, [u8]>)]) {
    for (kind, payload) in frames {
        let kind = match *kind {
            KIND_TEXT => "text",
            KIND_BLOB => "blob",
            _ => "?",
        };
        let storage = match payload {
            Cow::Borrowed(_) => "borrowed",
            Cow::Owned(_) => "owned",
        };
        println!(
            "  {source:>9}: kind={kind} len={} storage={storage} payload={:?}",
            payload.len(),
            String::from_utf8_lossy(payload)
        );
    }
}
