//! Instruction-count benchmarks (Valgrind/Callgrind), one shot per cell.
//!
//! These are deterministic where `benches/throughput.rs` is not, which is why they carry the
//! regression gate: soft limits are percentages against a saved baseline.
//!
//! ```console
//! cargo bench --bench instructions                              # measure, compare if a baseline exists
//! cargo bench --bench instructions -- --save-baseline=main      # store the baseline
//! cargo bench --bench instructions -- --baseline=main           # compare (exit 3 on a regression)
//! cargo bench --bench instructions -- --test                    # smoke: run each cell once, no valgrind
//! ```
//!
//! The `include!` keeps the harness at the crate root: `main!` only accepts a bare identifier for a
//! group and emits `main`, so wrapping it in a module would hide the entry point.

mod common;

#[cfg(target_os = "linux")]
include!("instructions/linux.rs");

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("skipping: gungraun drives valgrind, which is Linux-only");
}
