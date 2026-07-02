#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]

#[cfg(not(feature = "std"))]
compile_error!(
    "This library requires the `std` feature enabled until core::io::ErrorKind gets stabilized"
);

mod error;
pub use error::*;

mod read;
mod write;
pub use read::*;
pub use write::*;
