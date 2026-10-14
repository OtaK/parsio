use std::borrow::Cow;

use parsio::Read;

/// The `Cow` returned by `read_array`/`read_slice` borrows the *data*, not the reader, so it
/// must stay usable after the reader is dropped.
#[test]
fn borrowed_reads_outlive_the_reader() {
    let data = [1u8, 2, 3, 4];

    let (head, rest) = {
        let mut r: &[u8] = &data;
        let head: Cow<[u8; 2]> = r.read_array().unwrap();
        (head, r.read_till_eof().unwrap())
    };

    assert_eq!(&*head, &[1, 2]);
    assert_eq!(&*rest, &[3, 4]);
}
