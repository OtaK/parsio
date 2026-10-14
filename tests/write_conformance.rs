use std::collections::VecDeque;
use std::io::ErrorKind;

use parsio::{StdWriter, Write, WriteError, WriteResult};

fn assert_written(name: &str, op: &str, got: WriteResult<usize>, want: usize) {
    match got {
        Ok(n) => assert_eq!(n, want, "{name}: {op}"),
        Err(e) => panic!("{name}: {op}: expected Ok({want}), got Err({e:?})"),
    }
}

fn assert_write_zero<T: std::fmt::Debug>(name: &str, op: &str, got: WriteResult<T>) {
    match got {
        Err(WriteError::IoError(ErrorKind::WriteZero)) => {}
        other => panic!("{name}: {op}: expected IoError(WriteZero), got {other:?}"),
    }
}

fn expect_err<T: std::fmt::Debug>(name: &str, op: &str, got: WriteResult<T>, kind: ErrorKind) {
    match got {
        Err(WriteError::IoError(k)) => assert_eq!(k, kind, "{name}: {op}: wrong error kind"),
        other => panic!("{name}: {op}: expected Err({kind:?}), got {other:?}"),
    }
}

fn assert_ok<T: std::fmt::Debug>(name: &str, op: &str, got: WriteResult<T>) {
    match got {
        Ok(_) => {}
        Err(e) => panic!("{name}: {op}: expected Ok, got Err({e:?})"),
    }
}

fn fixed_case<W: Write>(name: &str, mut w: W) {
    assert_written(name, "write(3)", w.write(&[1, 2, 3]), 3);
    assert_written(
        name,
        "write_vectored(2,0,1)",
        w.write_vectored(&[&[4, 5], &[], &[6]]),
        3,
    );
    assert_written(name, "write(3) into 2 free bytes", w.write(&[7, 8, 9]), 2);
    assert_written(name, "write(&[])", w.write(&[]), 0);
    assert_written(name, "write_vectored(&[])", w.write_vectored(&[]), 0);
    assert_written(name, "write(1) into a full sink", w.write(&[9]), 0);
    assert_write_zero(name, "write_all(1) into a full sink", w.write_all(&[9]));
    assert_ok(name, "write_all(&[])", w.write_all(&[]));
    assert_ok(name, "flush", w.flush());
}

fn sink_case<W: Write>(name: &str, mut w: W) -> W {
    assert_written(name, "write(3)", w.write(&[1, 2, 3]), 3);
    assert_written(
        name,
        "write_vectored(2,0,1)",
        w.write_vectored(&[&[4, 5], &[], &[6]]),
        3,
    );
    assert_ok(name, "write_all(2)", w.write_all(&[7, 8]));
    assert_ok(name, "write_all(&[])", w.write_all(&[]));
    assert_written(name, "write(&[])", w.write(&[]), 0);
    assert_written(name, "write_vectored(&[])", w.write_vectored(&[]), 0);
    assert_ok(name, "flush", w.flush());
    w
}

const SINK_EXPECT: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

#[test]
fn conformance_slice_writer() {
    let mut buf = [0xffu8; 8];
    fixed_case("&mut [u8]", &mut buf[..]);
    assert_eq!(buf, SINK_EXPECT);
}

#[test]
fn conformance_boxed_slice_writer() {
    let mut buf = [0xffu8; 8];
    fixed_case("Box<&mut [u8]>", Box::new(&mut buf[..]));
    assert_eq!(buf, SINK_EXPECT);
}

#[test]
fn conformance_slice_writer_rejects_oversized_write_all() {
    let mut buf = [0xffu8; 4];

    {
        let mut w: &mut [u8] = &mut buf;
        assert_write_zero(
            "&mut [u8]",
            "write_all(5) into 4 bytes",
            w.write_all(&[1, 2, 3, 4, 5]),
        );
        assert_ok(
            "&mut [u8]",
            "write_all(4) afterwards",
            w.write_all(&[1, 2, 3, 4]),
        );
    }

    assert_eq!(
        buf,
        [1, 2, 3, 4],
        "&mut [u8]: nothing written by the failed call"
    );
}

#[test]
fn conformance_vec_writer() {
    let got = sink_case("Vec<u8>", Vec::new());
    assert_eq!(got, SINK_EXPECT);
}

#[test]
fn conformance_ref_mut_vec_writer() {
    let mut inner = Vec::new();

    {
        let w: &mut Vec<u8> = &mut inner;
        let _ = sink_case("&mut Vec<u8>", w);
    }

    assert_eq!(inner, SINK_EXPECT);
}

#[test]
fn conformance_std_writer() {
    let w = sink_case("StdWriter<Vec<u8>>", StdWriter::new(Vec::new()));
    assert_eq!(w.into_inner(), SINK_EXPECT);
}

/// A `parsio::Write` with no overrides, so the trait's default `write_all`
/// loop-based impl actually gets exercised
struct Mock {
    steps: VecDeque<MockStep>,
    calls: usize,
    written: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
enum MockStep {
    Wrote(usize),
    Interrupted,
    WouldBlock,
}

impl Mock {
    fn new(plan: impl IntoIterator<Item = MockStep>) -> Self {
        Self {
            steps: plan.into_iter().collect(),
            calls: 0,
            written: Vec::new(),
        }
    }
}

impl Write for Mock {
    fn write(&mut self, buf: &[u8]) -> WriteResult<usize> {
        self.calls += 1;
        match self.steps.pop_front().expect("no steps left, huh?") {
            MockStep::Wrote(n) => {
                let n = n.min(buf.len());
                self.written.extend_from_slice(&buf[..n]);
                Ok(n)
            }
            MockStep::Interrupted => Err(WriteError::IoError(ErrorKind::Interrupted)),
            MockStep::WouldBlock => Err(WriteError::IoError(ErrorKind::WouldBlock)),
        }
    }

    fn flush(&mut self) -> WriteResult<()> {
        Ok(())
    }

    fn write_vectored(&mut self, bufs: &[&[u8]]) -> WriteResult<usize> {
        let first = bufs.iter().find(|b| !b.is_empty()).copied().unwrap_or(&[]);
        self.write(first)
    }
}

#[test]
fn default_write_all_retries_interrupted() {
    let mut mock = Mock::new([MockStep::Interrupted, MockStep::Wrote(3)]);
    assert_ok(
        "Mock",
        "write_all(3) after Interrupted",
        mock.write_all(&[1, 2, 3]),
    );
    assert_eq!((mock.calls, mock.written.as_slice()), (2, &[1u8, 2, 3][..]));
}

#[test]
fn default_write_all_loops_on_short_writes() {
    let mut mock = Mock::new([MockStep::Wrote(1), MockStep::Wrote(1), MockStep::Wrote(1)]);
    assert_ok(
        "Mock",
        "write_all(3) over short writes",
        mock.write_all(&[1, 2, 3]),
    );
    assert_eq!((mock.calls, mock.written.as_slice()), (3, &[1u8, 2, 3][..]));
}

#[test]
fn default_write_all_reports_write_zero() {
    let mut mock = Mock::new([MockStep::Wrote(0)]);
    assert_write_zero(
        "Mock",
        "write_all(3) against Ok(0)",
        mock.write_all(&[1, 2, 3]),
    );
    assert_eq!(mock.calls, 1);
}

#[test]
fn default_write_all_propagates_other_errors() {
    // Interrupted is the only kind the default loop retries, inject something else to get a relevant err
    let mut mock = Mock::new([MockStep::WouldBlock]);
    expect_err(
        "Mock",
        "write_all(3)",
        mock.write_all(&[1, 2, 3]),
        ErrorKind::WouldBlock,
    );
    assert_eq!(mock.calls, 1);
}
