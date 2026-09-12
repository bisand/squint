//! squint's engine: a document that opens instantly however big the file is.
//!
//! - [`source`]: where the original bytes come from, read with `pread` so
//!   nothing is loaded that is not looked at.
//! - [`index`]: a sparse line index built incrementally, lifted from ctail.
//! - [`document`]: a piece table over the original and an add buffer, with
//!   line access, undo and atomic whole-file save.
//! - [`find`]: a search that walks the document in bounded steps.
//! - [`format`]: streaming JSON and XML pretty-printers.
//! - [`project`]: a source's formatted form as a source of its own, so a
//!   minified document can be read formatted without one being written.
//! - [`editorconfig`]: the `.editorconfig` lookup the formatters take their
//!   layout from.

pub mod document;
pub mod editorconfig;
pub mod find;
pub mod format;
pub mod index;
/// The formatted form of a source, measured but never written.
pub mod project;
pub mod source;
/// Syntax highlighting with syntect, behind the `syntax` feature.
#[cfg(feature = "syntax")]
pub mod syntax;

pub use document::Document;
pub use find::{Find, FindStep, Needle};
pub use index::LineIndex;
pub use source::{FileSource, MemSource, Source};
