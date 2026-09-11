//! squint's engine: a document that opens instantly however big the file is.
//!
//! - [`source`]: where the original bytes come from, read with `pread` so
//!   nothing is loaded that is not looked at.
//! - [`index`]: a sparse line index built incrementally, lifted from ctail.
//! - [`document`]: a piece table over the original and an add buffer, with
//!   line access, undo and atomic whole-file save.

pub mod document;
pub mod index;
pub mod source;

pub use document::Document;
pub use index::LineIndex;
pub use source::{FileSource, MemSource, Source};
