use std::panic::AssertUnwindSafe;

use parsio::{DepthAwareReader, Read, ReadError, ReadResult};

type Reader<'d> = DepthAwareReader<&'d [u8]>;

/// Enters nested levels until the limit puts a stop to it. Returns `(levels entered, reported error)`.
///
/// Chain shape: recursion, since a deeper guard borrows the one above — the stack of live guards
/// *is* the entered depth, which is exactly what the counter must reflect.
fn enter_until_overflow(reader: &mut Reader<'_>) -> (u16, (u16, u16)) {
    match reader.enter() {
        Ok(mut root) => enter_deeper(&mut root, 1),
        Err(ReadError::AllowedDepthOverflow { depth, limit }) => (0, (depth, limit)),
        Err(other) => panic!("unexpected error at depth 0: {other:?}"),
    }
}

fn enter_deeper<'a, 'd>(
    guard: &mut parsio::DepthAwareReaderGuard<'a, &'d [u8]>,
    entered: u16,
) -> (u16, (u16, u16)) {
    match guard.enter() {
        Ok(mut child) => enter_deeper(&mut child, entered + 1),
        Err(ReadError::AllowedDepthOverflow { depth, limit }) => (entered, (depth, limit)),
        Err(other) => panic!("unexpected error at depth {entered}: {other:?}"),
    }
}

#[test]
fn depth_limit_matrix() {
    let data = b"abcdef";

    for limit in [0, 1, 2, 3, 8, DepthAwareReader::<&[u8]>::DEFAULT_LIMIT] {
        let mut reader = DepthAwareReader::from_reader_with_limit(&data[..], limit);

        if limit == 0 {
            // No level can be entered at all, so the reader must stay at depth 0: repeating the
            // attempt has to keep reporting the same depth+1.
            let mut reported = Vec::new();
            for _ in 0..3 {
                match reader.enter() {
                    Err(ReadError::AllowedDepthOverflow { depth, limit }) => {
                        reported.push((depth, limit))
                    }
                    other => panic!("limit 0: expected overflow, got {}", other.is_ok()),
                }
            }
            assert_eq!(
                reported,
                vec![(1, 0); 3],
                "limit 0: reported depths across retries"
            );
            continue;
        }

        let (entered, (depth, reported_limit)) = enter_until_overflow(&mut reader);
        assert_eq!(entered, limit, "limit {limit}: levels entered");
        assert_eq!(
            (depth, reported_limit),
            (limit + 1, limit),
            "limit {limit}: overflow error payload"
        );

        // Guards dropped => the whole budget is available again
        let (entered_again, _) = enter_until_overflow(&mut reader);
        assert_eq!(
            entered_again, limit,
            "limit {limit}: budget after unwinding"
        );

        // The stream itself is untouched by any of this
        assert_eq!(
            reader.read_byte().unwrap(),
            b'a',
            "limit {limit}: stream position"
        );
    }
}

#[test]
fn depth_budget_restored_after_panic() {
    let data = b"abcdef";
    let limit = 3;
    let mut reader = DepthAwareReader::from_reader_with_limit(&data[..], limit);

    let unwind = std::panic::catch_unwind(AssertUnwindSafe(|| {
        // Chain via shadowing: both guards stay live until the unwind drops them in reverse
        // order, each releasing exactly its own level.
        let mut guard = reader.enter().unwrap();
        let mut _guard = guard.enter().unwrap();
        panic!("unwind with two live guards");
    }));
    assert!(unwind.is_err(), "expected the closure to panic");

    let (entered, _) = enter_until_overflow(&mut reader);
    assert_eq!(entered, limit, "budget must be whole again after an unwind");
}

/// A guard takes the reader's only `&mut`, so reading at an entered level goes through the guard.
#[test]
fn reads_happen_through_the_guard() {
    let data = b"head,tail";
    let mut reader = DepthAwareReader::from_reader(&data[..]);

    {
        let mut guard = reader.enter().unwrap();

        assert_eq!(guard.peek_byte().unwrap(), b'h', "peek through the guard");
        assert_eq!(guard.peek_slice(), data, "the window forwards too");

        let (field, found) = guard.read_until_match(b",").unwrap();
        assert_eq!((field.as_ref(), found), (&b"head"[..], true));

        let rest = guard.read_till_eof().unwrap();
        assert_eq!(rest.as_ref(), b"tail");
    }

    // Guard dropped: the budget is whole again, and nothing was consumed twice.
    assert!(
        reader.enter().is_ok(),
        "budget available once the guard is gone"
    );
}

/// The threat the guard is for: nesting deep enough to blow up a recursive parser. Each level
/// reads *through* its own guard, and the limit stops the descent.
///
/// Chain shape: the child borrows the parent, so the whole chain of guards is live while the
/// recursion descends — exactly the levels the parse has actually entered.
fn parse_chain<'a, 'data, R: Read<'data>>(
    guard: &mut parsio::DepthAwareReaderGuard<'a, R>,
) -> ReadResult<usize> {
    let tag = guard.read_byte()?;
    if tag == 0 {
        return Ok(1);
    }
    let mut child = guard.enter()?;
    Ok(1 + parse_chain(&mut child)?)
}

#[test]
fn nested_reads_trip_the_limit() {
    let data = [1u8, 1, 1, 1, 0];
    let mut reader = DepthAwareReader::from_reader_with_limit(&data[..], 2);

    let mut root = reader.enter().unwrap();
    let result = parse_chain(&mut root);
    match result {
        Err(ReadError::AllowedDepthOverflow { depth, limit }) => {
            assert_eq!(
                (depth, limit),
                (3, 2),
                "reads counted at the level that made them"
            );
        }
        other => panic!("expected the descent to be cut short, got {other:?}"),
    }

    // The unwinding restored the budget, so a shallower parse still fits: four nested tags and
    // the terminator, read one level each.
    let mut reader = DepthAwareReader::from_reader_with_limit(&data[..], 5);
    {
        let mut root = reader.enter().unwrap();
        assert_eq!(parse_chain(&mut root).unwrap(), 5, "levels read");
    }
    assert!(reader.read_byte().is_err(), "the stream is at its end");
}

/// Fan-out shape: a level with several children, each itself entered. The old API could not
/// express this *and* count levels: holding the parent guard across children was E0382, and
/// dropping it to recurse counted nothing.
fn parse_fanout<'a, 'data, R: Read<'data>>(
    guard: &mut parsio::DepthAwareReaderGuard<'a, R>,
) -> ReadResult<usize> {
    let children = guard.read_byte()?;
    let mut total = 1;
    for _ in 0..children {
        let mut child = guard.enter()?;
        total += parse_fanout(&mut child)?;
    }
    Ok(total)
}

#[test]
fn fanout_reads_trip_the_limit() {
    // Depth-3 tree: two levels of branching, leaves at the third.
    let data = [2u8, 2, 0, 0, 2, 0, 0];
    let mut reader = DepthAwareReader::from_reader_with_limit(&data[..], 2);

    {
        let mut root = reader.enter().unwrap();
        let result = parse_fanout(&mut root);
        match result {
            Err(ReadError::AllowedDepthOverflow { depth, limit }) => {
                assert_eq!(
                    (depth, limit),
                    (3, 2),
                    "the fan-out depth must count: children entered from a held parent"
                );
            }
            other => panic!("expected the fan-out to be cut short, got {other:?}"),
        }
    }

    // The unwind released every level, so a shallower fan-out fits.
    let mut reader = DepthAwareReader::from_reader_with_limit(&data[..], 3);
    {
        let mut root = reader.enter().unwrap();
        assert_eq!(parse_fanout(&mut root).unwrap(), 7, "nodes read");
    }
    assert!(reader.read_byte().is_err(), "the stream is at its end");
}

#[test]
fn fanout_parent_resumes_between_children() {
    // Parent reads its own bytes interleaved with children: it must be usable again after each
    // child drops, and the counter must show exactly one level after every child returns.
    let data = [2u8, 0, 0];
    let mut reader = DepthAwareReader::from_reader_with_limit(&data[..], 2);

    let mut root = reader.enter().unwrap();
    assert_eq!(root.read_byte().unwrap(), 2, "child count");

    for i in 0..2 {
        let mut child = root.enter().unwrap();
        assert_eq!(child.depth(), 2, "child {i} entered: two live levels");
        assert_eq!(child.read_byte().unwrap(), 0, "child {i} leaf tag");
    }

    assert_eq!(root.depth(), 1, "children dropped: parent level only");
    assert_eq!(root.limit(), 2);
    assert!(root.read_byte().is_err(), "the stream is fully consumed");
}

#[test]
fn fanout_budget_restored_after_panic() {
    let data = [2u8, 2, 0, 0, 2, 0, 0];
    let mut reader = DepthAwareReader::from_reader_with_limit(&data[..], 2);

    let unwind = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut root = reader.enter().unwrap();
        let mut child = root.enter().unwrap();
        let mut _grandchild = child.enter().unwrap();
        // `_grandchild` stays live until the unwind drops all three guards in reverse order.
        panic!("unwind mid-fanout");
    }));
    assert!(unwind.is_err(), "expected the closure to panic");

    assert_eq!(reader.depth(), 0, "every level released by the unwind");
}

/// A failed `enter` claims nothing: the level it was called from must still be readable, and
/// its own budget intact. Reader-level retries are covered by `depth_limit_matrix`; this pins
/// the guard-level case that a fan-out parser lives on — catch the overflow, skip the subtree,
/// keep reading siblings.
#[test]
fn guard_readable_after_failed_child_enter() {
    let data = [2u8, 0, 1, 0];
    let mut reader = DepthAwareReader::from_reader_with_limit(&data[..], 1);

    let mut root = reader.enter().unwrap();
    assert_eq!(root.read_byte().unwrap(), 2, "child count");

    // With limit 1 every child `enter` must fail without claiming anything.
    for _ in 0..2 {
        match root.enter() {
            Ok(_) => panic!("limit 1 must refuse a second level"),
            Err(ReadError::AllowedDepthOverflow { depth, limit }) => {
                assert_eq!((depth, limit), (2, 1));
            }
            Err(other) => panic!("unexpected error: {other:?}"),
        }
        // The parent guard survived the failed enters: still readable, still at its level.
        assert_eq!(root.depth(), 1, "failed enter claims no level");
    }

    // And it can still consume its own bytes: the two child tags plus the trailing tag remain.
    assert_eq!(root.read_byte().unwrap(), 0, "first child tag");
    assert_eq!(root.read_byte().unwrap(), 1, "second child tag");
    assert_eq!(root.read_byte().unwrap(), 0, "trailing tag");
    assert!(root.read_byte().is_err(), "stream consumed");
    assert_eq!(root.depth(), 1);
}

/// The ext helpers build the same adapters as the constructors: `limited` wraps in a `Take`,
/// `with_max_depth` wraps in a `DepthAwareReader` with the limit intact.
#[test]
fn ext_helpers_compose() {
    use parsio::{DepthLimitedReadExt, LimitedReadExt};

    let data = b"keep,drop";

    // limited() over a plain slice
    let mut reader = &data[..];
    let mut take = reader.limited(5);
    let (field, found) = take.read_until_match(b",").unwrap();
    assert_eq!((field.as_ref(), found), (&b"keep"[..], true));

    // limited() over a guard, and nested: every `Read` gets the ext traits
    let mut reader = DepthAwareReader::from_reader(&data[..]);
    let mut root = reader.enter().unwrap();
    let mut outer = root.limited(4);
    let mut take = outer.limited(2);
    let (field, found) = take.read_until_match(b",").unwrap();
    assert_eq!((field.as_ref(), found), (&b"ke"[..], false));

    // with_max_depth() over a plain slice: the depth limit is live inside the wrapper
    let mut reader = &data[..];
    let mut limited = reader.with_max_depth(1);
    {
        let mut outer = limited.enter().unwrap();
        match outer.enter() {
            Ok(_) => panic!("limit 1 must refuse a second level"),
            Err(ReadError::AllowedDepthOverflow { depth, limit }) => {
                assert_eq!((depth, limit), (2, 1));
            }
            Err(other) => panic!("unexpected error: {other:?}"),
        }
        // The reading surface still forwards through the depth wrapper
        assert_eq!(outer.read_byte().unwrap(), b'k');
    }
    assert_eq!(limited.read_byte().unwrap(), b'e');
}

/// A guard is a reader, so adapters compose around it.
#[test]
fn take_over_a_guard_is_bounded() {
    let data = b"keep,drop";
    let mut reader = DepthAwareReader::from_reader(&data[..]);

    {
        let guard = reader.enter().unwrap();
        // Five bytes: the four of "keep" plus the delimiter.
        let mut take = parsio::Take::new(guard, 5);

        let (field, found) = take.read_until_match(b",").unwrap();
        assert_eq!((field.as_ref(), found), (&b"keep"[..], true));
        assert!(take.read_byte().is_err(), "the limit still holds");
    }

    // A limit stopping short of the delimiter leaves the field unterminated, as it should.
    let mut reader = DepthAwareReader::from_reader(&data[..]);
    let guard = reader.enter().unwrap();
    let mut take = parsio::Take::new(guard, 4);
    let (field, found) = take.read_until_match(b",").unwrap();
    assert_eq!((field.as_ref(), found), (&b"keep"[..], false));
}

/// `limit == u16::MAX` used to wrap the counter back to 0 in release, disabling the guard
#[test]
fn max_limit_does_not_wrap() {
    let mut reader = DepthAwareReader::from_reader_with_limit(&b""[..], u16::MAX);
    // Leak guards to reach the top without 65k stack frames
    for _ in 0..u16::MAX {
        std::mem::forget(reader.enter().unwrap());
    }
    assert_eq!(reader.depth(), u16::MAX);
    assert!(matches!(
        reader.enter(),
        Err(ReadError::AllowedDepthOverflow {
            depth: u16::MAX,
            limit: u16::MAX
        })
    ));
    assert_eq!(reader.depth(), u16::MAX);
}
