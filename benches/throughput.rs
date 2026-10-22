//! Wall-clock benchmarks (criterion).
//!
//! For the regression gate, see `benches/instructions.rs`: instruction counts from Callgrind are
//! deterministic, wall-clock times on a shared machine are not.
//!
//! ```console
//! cargo bench --bench throughput              # everything
//! cargo bench --bench throughput -- read_until/take   # one group
//! cargo bench --bench throughput -- --test            # smoke: run each cell once, no measuring
//! ```

mod common;

use std::borrow::Cow;
use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use parsio::{DepthAwareReader, Read, Take, Write};

use common::{ABSENT, ONE, TWO, Windowless};

/// Bytes in the streams under test. Big enough that a per-field cost dominates the loop overhead,
/// small enough that criterion can iterate it many times.
const STREAM: usize = 1 << 20;

fn stream_of(field: usize, needle: &[u8]) -> Vec<u8> {
    if needle == TWO {
        common::lines(field, STREAM / (field + 2))
    } else {
        common::fields(field, STREAM / (field + 1))
    }
}

fn read_until(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_until");

    // Feeding the same bytes through every impl: the interesting differences are in how each one
    // finds the needle and hands out the bytes, not in what the data is.
    for field in [20usize, 512, 8 << 10] {
        let data = stream_of(field, ONE);

        for (name, needle) in [("1 byte", ONE), ("2 bytes", TWO)] {
            let data = stream_of(field, needle);
            group.throughput(Throughput::Bytes(data.len() as u64));

            group.bench_with_input(
                BenchmarkId::new(format!("&[u8]/{name}"), field),
                &data,
                |b, data| b.iter(|| black_box(common::drain(&data[..], black_box(needle)))),
            );
            group.bench_with_input(
                BenchmarkId::new(format!("StdReader 8k/{name}"), field),
                &data,
                |b, data| {
                    b.iter(|| {
                        black_box(common::drain(
                            common::std_reader(data, 8 << 10),
                            black_box(needle),
                        ))
                    })
                },
            );
            group.bench_with_input(
                BenchmarkId::new(format!("Take<&[u8]>/{name}"), field),
                &data,
                |b, data| {
                    b.iter(|| {
                        let reader = Take::new(black_box(&data[..]), data.len());
                        black_box(common::drain(reader, black_box(needle)))
                    })
                },
            );
            group.bench_with_input(
                BenchmarkId::new(format!("Take<StdReader 8k>/{name}"), field),
                &data,
                |b, data| {
                    b.iter(|| {
                        black_box(common::drain(
                            Take::new(common::std_reader(data, 8 << 10), data.len()),
                            black_box(needle),
                        ))
                    })
                },
            );
        }
        group.throughput(Throughput::Bytes(data.len() as u64));

        // Destructive readers: the buffer is drained, so every iteration needs its own copy.
        group.bench_with_input(BenchmarkId::new("Vec<u8>", field), &data, |b, data| {
            b.iter_batched(
                || data.clone(),
                |data| black_box(common::drain(data, black_box(ONE))),
                BatchSize::LargeInput,
            )
        });
        group.bench_with_input(
            BenchmarkId::new("Take<Vec<u8>>", field),
            &data,
            |b, data| {
                b.iter_batched(
                    || data.clone(),
                    |data: Vec<u8>| {
                        let limit = data.len();
                        black_box(common::drain(Take::new(data, limit), black_box(ONE)))
                    },
                    BatchSize::LargeInput,
                )
            },
        );
        group.bench_with_input(BenchmarkId::new("Cow::Owned", field), &data, |b, data| {
            b.iter_batched(
                || Cow::Owned(data.clone()),
                |data| black_box(common::drain(data, black_box(ONE))),
                BatchSize::LargeInput,
            )
        });
    }

    // Borrowing readers, one cell each: dispatch and borrow bookkeeping only.
    let data = stream_of(20, ONE);
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_function("Cow::Borrowed/20", |b| {
        b.iter(|| black_box(common::drain(Cow::Borrowed(&data[..]), black_box(ONE))))
    });
    group.bench_function("DepthAwareReader<&[u8]>/20", |b| {
        b.iter(|| {
            black_box(common::drain(
                DepthAwareReader::from_reader(black_box(&data[..])),
                black_box(ONE),
            ))
        })
    });

    // A needle that is never there: the scan covers the whole stream, which is where the bulk
    // paths pay off most.
    group.bench_function("&[u8]/absent", |b| {
        b.iter(|| black_box(common::drain(black_box(&data[..]), black_box(ABSENT))))
    });
    group.bench_function("Take<&[u8]>/absent", |b| {
        b.iter(|| {
            let reader = Take::new(black_box(&data[..]), data.len());
            black_box(common::drain(reader, black_box(ABSENT)))
        })
    });

    group.finish();
}

/// The refill and seam paths: tiny buffers make windows shorter than the needle, and drain them
/// between reads.
fn read_until_tiny_windows(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_until/tiny-window");

    for capacity in [1usize, 3] {
        let data = common::lines(20, 4 << 10);
        group.throughput(Throughput::Bytes(data.len() as u64));
        group.bench_with_input(BenchmarkId::new("StdReader", capacity), &data, |b, data| {
            b.iter(|| {
                black_box(common::drain(
                    common::std_reader(data, capacity),
                    black_box(TWO),
                ))
            })
        });
        group.bench_with_input(
            BenchmarkId::new("Take<StdReader>", capacity),
            &data,
            |b, data| {
                b.iter(|| {
                    black_box(common::drain(
                        Take::new(common::std_reader(data, capacity), data.len()),
                        black_box(TWO),
                    ))
                })
            },
        );
    }

    // No window at all: `Take` walks it a byte at a time.
    let data = stream_of(20, ONE);
    group.throughput(Throughput::Bytes(data.len() as u64));
    group.bench_with_input(
        BenchmarkId::new("Take<Windowless>", 20),
        &data,
        |b, data| {
            b.iter(|| {
                let reader = Take::new(Windowless(black_box(&data[..])), data.len());
                black_box(common::drain(reader, black_box(ONE)))
            })
        },
    );

    group.finish();
}

/// A limit that cuts mid-field, and one that hides the needle: the adapter's clamp has to hold
/// either way.
fn take_limits(c: &mut Criterion) {
    let mut group = c.benchmark_group("take-limits");

    let data = stream_of(20, ONE);
    group.throughput(Throughput::Bytes(data.len() as u64));

    // Half the stream: every field inside the window, then EOF mid-field.
    let half = data.len() / 2;
    group.bench_function("limit cuts mid-field", |b| {
        b.iter(|| {
            let reader = Take::new(black_box(&data[..]), half);
            black_box(common::drain(reader, black_box(ONE)))
        })
    });

    // The needle sits beyond the limit, so the whole window is one unmatched field.
    let mut hidden = common::fields(20, 64);
    hidden.extend_from_slice(b"#");
    group.bench_function("needle beyond the limit", |b| {
        b.iter(|| {
            let limit = hidden.len() - 1;
            let reader = Take::new(black_box(&hidden[..]), limit);
            black_box(common::drain(reader, black_box(b"#")))
        })
    });

    group.finish();
}

/// The per-byte primitives the scans are built on: the baseline any scan has to beat.
fn primitives(c: &mut Criterion) {
    let mut group = c.benchmark_group("primitives");
    let data = common::cycled(1 << 16);
    group.throughput(Throughput::Bytes(data.len() as u64));

    group.bench_function("&[u8]/read_byte", |b| {
        b.iter(|| {
            let mut reader = black_box(&data[..]);
            let mut total = 0u64;
            while let Ok(byte) = reader.read_byte() {
                total += u64::from(black_box(byte));
            }
            black_box(total)
        })
    });
    group.bench_function("StdReader 8k/read_byte", |b| {
        b.iter(|| {
            let mut reader = common::std_reader(black_box(&data), 8 << 10);
            let mut total = 0u64;
            while let Ok(byte) = reader.read_byte() {
                total += u64::from(black_box(byte));
            }
            black_box(total)
        })
    });
    group.bench_function("&[u8]/advance+peek", |b| {
        b.iter(|| {
            let mut reader = black_box(&data[..]);
            let mut total = 0u64;
            while let Ok(byte) = reader.peek_byte() {
                reader.advance(1).unwrap();
                total += u64::from(byte);
            }
            black_box(total)
        })
    });
    group.bench_function("StdReader 8k/advance+peek", |b| {
        b.iter(|| {
            let mut reader = common::std_reader(black_box(&data), 8 << 10);
            let mut total = 0u64;
            while let Ok(byte) = reader.peek_byte() {
                reader.advance(1).unwrap();
                total += u64::from(byte);
            }
            black_box(total)
        })
    });

    group.bench_function("&[u8]/read_slice(512)", |b| {
        b.iter(|| {
            let mut reader = black_box(&data[..]);
            let mut total = 0;
            while let Ok(chunk) = reader.read_slice(512) {
                // A slice read is a pointer bump: without this the loop collapses into arithmetic.
                total += black_box(chunk.len());
            }
            black_box(total)
        })
    });
    group.bench_function("StdReader 8k/read_slice(512)", |b| {
        b.iter(|| {
            let mut reader = common::std_reader(black_box(&data), 8 << 10);
            let mut total = 0;
            while let Ok(chunk) = reader.read_slice(512) {
                total += black_box(chunk.len());
            }
            black_box(total)
        })
    });
    group.bench_function("&[u8]/read_array::<8>", |b| {
        b.iter(|| {
            let mut reader = black_box(&data[..]);
            let mut total = 0u64;
            while let Ok(chunk) = reader.read_array::<8>() {
                total += u64::from(chunk[0]);
            }
            black_box(total)
        })
    });
    group.bench_function("StdReader 8k/read_array::<8>", |b| {
        b.iter(|| {
            let mut reader = common::std_reader(black_box(&data), 8 << 10);
            let mut total = 0u64;
            while let Ok(chunk) = reader.read_array::<8>() {
                total += u64::from(chunk[0]);
            }
            black_box(total)
        })
    });

    group.bench_function("Take<&[u8]>/read_till_eof", |b| {
        b.iter(|| {
            let mut reader = Take::new(black_box(&data[..]), data.len());
            black_box(reader.read_till_eof().expect("bounded read").len())
        })
    });

    group.finish();
}

/// The billion-laughs guard: entering and leaving a depth level per nested value.
fn depth(c: &mut Criterion) {
    let mut group = c.benchmark_group("depth");
    let data = stream_of(20, ONE);
    group.throughput(Throughput::Bytes(data.len() as u64));

    // The shape the guard is for: each field is a nested value, read through the guard that
    // entered it. The `enter`/drop cost is what sits on top of the plain read.
    group.bench_function("enter + read through the guard", |b| {
        b.iter(|| {
            let mut reader = DepthAwareReader::from_reader(black_box(&data[..]));
            let mut total = 0;
            while let Ok((field, true)) = read_field_under_guard(&mut reader) {
                total += field.len();
            }
            black_box(total)
        })
    });

    group.bench_function("&[u8] baseline (no guard)", |b| {
        b.iter(|| black_box(common::drain(black_box(&data[..]), black_box(ONE))))
    });

    group.finish();
}

/// Reads one field at an entered depth, through the guard, dropping it on the way out.
fn read_field_under_guard<'d, R: Read<'d>>(
    reader: &mut DepthAwareReader<R>,
) -> parsio::ReadResult<(Cow<'d, [u8]>, bool)> {
    let mut guard = reader.enter().expect("within the default limit");
    guard.read_until_match(ONE)
}

fn write_side(c: &mut Criterion) {
    let mut group = c.benchmark_group("write");
    let payload = common::cycled(512);
    group.throughput(Throughput::Bytes(payload.len() as u64));

    group.bench_function("Vec<u8>/write(512)", |b| {
        b.iter_batched(
            Vec::new,
            |mut sink| {
                let written = sink.write(black_box(&payload)).unwrap();
                black_box((sink, written))
            },
            BatchSize::SmallInput,
        )
    });
    group.bench_function("Vec<u8>/write_all(512)", |b| {
        b.iter_batched(
            Vec::new,
            |mut sink| {
                sink.write_all(black_box(&payload)).unwrap();
                black_box(sink)
            },
            BatchSize::SmallInput,
        )
    });
    group.bench_function("Vec<u8>/write_vectored(2x256)", |b| {
        let (head, tail) = payload.split_at(256);
        b.iter_batched(
            Vec::new,
            |mut sink| {
                let written = sink
                    .write_vectored(&[black_box(head), black_box(tail)])
                    .unwrap();
                black_box((sink, written))
            },
            BatchSize::SmallInput,
        )
    });

    group.bench_function("&mut [u8]/write_all(512)", |b| {
        let mut buffer = vec![0u8; payload.len()];
        b.iter(|| {
            let mut sink = &mut buffer[..];
            sink.write_all(black_box(&payload)).unwrap();
            black_box(sink.len())
        })
    });
    group.bench_function("&mut [u8]/write_all(513) -> overflow", |b| {
        let mut buffer = vec![0u8; payload.len()];
        let too_long = common::cycled(payload.len() + 1);
        b.iter(|| {
            let mut sink = &mut buffer[..];
            black_box(sink.write_all(black_box(&too_long)).is_err())
        })
    });

    group.bench_function("StdWriter<Vec<u8>>/write_all(512)", |b| {
        b.iter_batched(
            || parsio::StdWriter::from(Vec::new()),
            |mut sink| {
                sink.write_all(black_box(&payload)).unwrap();
                black_box(sink)
            },
            BatchSize::SmallInput,
        )
    });
    group.bench_function("StdWriter<BufWriter>/write_all(512)", |b| {
        b.iter_batched(
            || parsio::StdWriter::from(std::io::BufWriter::with_capacity(8 << 10, Vec::new())),
            |mut sink| {
                sink.write_all(black_box(&payload)).unwrap();
                black_box(sink)
            },
            BatchSize::SmallInput,
        )
    });
    group.bench_function("StdWriter<Sink>/write_vectored(2x256)", |b| {
        let (head, tail) = payload.split_at(256);
        let mut sink = parsio::StdWriter::from(std::io::sink());
        b.iter(|| {
            black_box(
                sink.write_vectored(&[black_box(head), black_box(tail)])
                    .unwrap(),
            )
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    read_until,
    read_until_tiny_windows,
    take_limits,
    primitives,
    depth,
    write_side
);
criterion_main!(benches);
