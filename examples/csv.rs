//! A minimal CSV reader: `cargo run --example csv`.
//!
//! Deliberately naive — comma, LF/CRLF, no quoting — so the example stays about I/O rather than
//! about CSV's quoting rules.
//!
//! One [`Read::read_until`] call per line finds the line ending, then the fields are split out of
//! that line with plain slice iteration. On a slice both the line and every field are *borrows* of
//! the input; behind a `std::io::Read` such as a `File` the line is owned and the fields are
//! slices of it, so they are handed to the visitor within the same iteration.
//!
//! The needle is a byte slice, so it covers more than CSV's single byte: `b"\r\n"`, a multi-byte
//! UTF-8 delimiter, or `b"\n"` as here. Note what it does *not* cover: a *set* of delimiters. CSV
//! needs `,` or `\n`, which is why this version reads whole lines and splits them, instead of
//! scanning for one separator at a time.

use std::borrow::Cow;
use std::io::BufReader;

use parsio::{Read, ReadResult, StdReader};

const LINE_END: &[u8] = b"\n";
const SEPARATOR: u8 = b',';

/// Feeds every row's fields to `visit`, along with whether the row itself was borrowed.
///
/// Streaming (no `Vec` of rows) so the borrowed and owned cases can share one code path: an owned
/// line only outlives the call to `visit`.
fn for_each_row<'d, R: Read<'d>>(
    mut reader: R,
    mut visit: impl FnMut(usize, &[&[u8]], bool),
) -> ReadResult<usize> {
    let mut rows = 0;

    loop {
        let (line, found) = reader.read_until(LINE_END)?;
        if !found && line.is_empty() {
            return Ok(rows);
        }

        let borrowed = matches!(line, Cow::Borrowed(_));
        let line: &[u8] = match &line {
            Cow::Borrowed(bytes) => bytes,
            Cow::Owned(bytes) => bytes,
        };
        let line = line.strip_suffix(b"\r").unwrap_or(line);

        let fields: Vec<&[u8]> = line.split(|byte| *byte == SEPARATOR).collect();
        visit(rows, &fields, borrowed);
        rows += 1;
    }
}

fn main() {
    let input: &[u8] = b"name,age,city\r\nada,36,london\n\ngrace,45,new york\nno,trailing,newline";

    println!("&[u8]:");
    let rows = for_each_row(input, |index, fields, borrowed| {
        let storage = if borrowed { "borrowed" } else { "owned" };
        println!("  row {index} ({storage}): {}", render(fields));
    })
    .expect("in-memory parse");
    println!("  {rows} rows");

    // Same code, any `std::io::Read`; a one-byte buffer so every line crosses a refill.
    println!("\nStdReader:");
    let rows = for_each_row(
        StdReader::new(BufReader::with_capacity(1, input)),
        |index, fields, borrowed| {
            let storage = if borrowed { "borrowed" } else { "owned" };
            println!("  row {index} ({storage}): {}", render(fields));
        },
    )
    .expect("stream parse");
    println!("  {rows} rows");

    // What this naive reader does *not* do: quoted fields. `"two, three"` splits in two, which is
    // why real CSV needs a state machine on top of this crate rather than inside it.
    let quoted = b"1,\"two, three\",4\n";
    let mut count = 0;
    for_each_row(&quoted[..], |_, fields, _| count = fields.len()).expect("quoted parse");
    println!("\nquoted input, naive split: {count} fields");

    // Multi-byte needles work the same way: `b"\r\n"` is a two-byte needle.
    let mut reader: &[u8] = b"header\r\nbody";
    let (line, found) = reader.read_until(b"\r\n").unwrap();
    println!(
        "multi-byte needle b\"\\r\\n\": {:?} (found: {found}), rest: {:?}",
        String::from_utf8_lossy(&line),
        String::from_utf8_lossy(reader)
    );
}

fn render(fields: &[&[u8]]) -> String {
    let rendered: Vec<String> = fields
        .iter()
        .map(|field| format!("{:?}", String::from_utf8_lossy(field)))
        .collect();
    format!("[{}]", rendered.join(", "))
}
