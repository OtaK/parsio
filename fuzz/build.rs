//! Embeds the committed corpus into the test binary.
//!
//! Reading `fuzz/corpus/**` at runtime would need `-Zmiri-disable-isolation` for the Miri replay,
//! so the files are inlined here at build time instead. Build scripts run on the host, so this is
//! invisible to the isolated interpreter.

use std::fmt::Write as _;
use std::path::Path;

const TARGETS: [&str; 4] = [
    "read_ops",
    "std_reader_adversarial",
    "write_sinks",
    "depth_accounting",
];

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let corpus = Path::new(&manifest).join("corpus");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");

    println!("cargo:rerun-if-changed=corpus");

    let mut generated = String::from("/// (target, input) pairs, embedded at build time.\n");
    generated.push_str("pub const CORPUS: &[(&str, &[u8])] = &[\n");

    for target in TARGETS {
        let dir = corpus.join(target);
        println!("cargo:rerun-if-changed=corpus/{target}");

        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        files.sort();

        assert!(
            !files.is_empty(),
            "corpus/{target} has no seeds; the replay test would be vacuous"
        );

        for file in files {
            writeln!(
                generated,
                "    ({target:?}, include_bytes!({:?})),",
                file.to_str().expect("corpus path must be valid UTF-8")
            )
            .expect("write to string");
        }
    }

    generated.push_str("];\n");

    std::fs::write(Path::new(&out_dir).join("corpus_generated.rs"), generated)
        .expect("write corpus_generated.rs");
}
