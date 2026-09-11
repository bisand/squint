//! Pretty-printing JSON and XML as a stream.
//!
//! A minified document is one enormous line, the one shape a line index
//! cannot page through. These formatters turn it into lines without building
//! a tree: they are push parsers over bytes, fed a chunk at a time, holding a
//! nesting depth (JSON) or a flag per open element (XML) and nothing that
//! grows with the size of the document. A [`Format`] drives one across a
//! [`Document`] in bounded steps and writes as it goes, so a front end can
//! format gigabytes between frames and open the result like any other file.
//!
//! Both change only the whitespace between tokens; every byte of every token
//! comes out as it went in. Neither validates: input that is not well formed
//! comes out laid out as far as it could be read, never rejected.

mod json;
mod xml;

pub use json::JsonFormatter;
pub use xml::XmlFormatter;

use crate::document::Document;
use std::io::{self, Write};
use std::path::Path;

/// What a document is formatted as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Json,
    Xml,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Json => "JSON",
            Kind::Xml => "XML",
        }
    }
}

/// One level of indentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indent {
    Spaces(u8),
    Tab,
}

impl Default for Indent {
    /// Two spaces: what JSON and XML tooling most often writes.
    fn default() -> Self {
        Indent::Spaces(2)
    }
}

impl Indent {
    /// Starts a new line `depth` levels in.
    pub(crate) fn newline(self, depth: usize, out: &mut Vec<u8>) {
        out.push(b'\n');
        let (byte, count) = match self {
            Indent::Spaces(n) => (b' ', depth * n as usize),
            Indent::Tab => (b'\t', depth),
        };
        out.resize(out.len() + count, byte);
    }
}

/// A push formatter: bytes in, formatted bytes out, a chunk at a time.
///
/// Feeding the same input in any chunking gives the same output.
pub trait Formatter {
    /// Formats `input`, appending to `out`.
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>);

    /// Ends the input, appending whatever was held back and a final newline.
    fn finish(&mut self, out: &mut Vec<u8>);
}

/// A formatter for `kind`.
pub fn formatter(kind: Kind, indent: Indent) -> Box<dyn Formatter + Send> {
    match kind {
        Kind::Json => Box::new(JsonFormatter::new(indent)),
        Kind::Xml => Box::new(XmlFormatter::new(indent)),
    }
}

/// What a file can be formatted as: by its first byte that is not
/// whitespace, which is conclusive for the documents that need formatting,
/// and failing that by its extension.
pub fn detect(path: Option<&Path>, head: &[u8]) -> Option<Kind> {
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    match head.iter().find(|b| !b.is_ascii_whitespace()) {
        Some(b'{' | b'[') => return Some(Kind::Json),
        Some(b'<') => return Some(Kind::Xml),
        _ => {}
    }
    let ext = path
        .and_then(Path::extension)
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("json" | "jsonl" | "ndjson" | "geojson" | "har") => Some(Kind::Json),
        Some(
            "xml" | "xsd" | "xsl" | "xslt" | "svg" | "xhtml" | "rss" | "atom" | "xaml" | "wsdl",
        ) => Some(Kind::Xml),
        _ => None,
    }
}

/// Bytes read from the document per feed.
const CHUNK: usize = 1024 * 1024;

/// A document being formatted, a step at a time.
///
/// Reads the document as it is, edits included; a front end that lets the
/// text change underneath it should start again.
pub struct Format {
    formatter: Box<dyn Formatter + Send>,
    kind: Kind,
    pos: u64,
    len: u64,
    input: Vec<u8>,
    output: Vec<u8>,
    done: bool,
}

impl Format {
    pub fn new(doc: &Document, kind: Kind, indent: Indent) -> Self {
        Self {
            formatter: formatter(kind, indent),
            kind,
            pos: 0,
            len: doc.len(),
            input: Vec::new(),
            output: Vec::new(),
            done: false,
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Bytes of the document formatted so far, out of its length.
    pub fn progress(&self) -> (u64, u64) {
        (self.pos.min(self.len), self.len)
    }

    /// Formats up to `budget` more bytes of `doc` into `w`. Returns whether
    /// the format has finished, which is when everything is written.
    pub fn step<W: Write + ?Sized>(
        &mut self,
        doc: &Document,
        budget: usize,
        w: &mut W,
    ) -> io::Result<bool> {
        let mut left = budget.max(1);
        while !self.done && left > 0 {
            self.input.resize(left.min(CHUNK), 0);
            let n = doc.read_at(self.pos, &mut self.input)?;
            self.output.clear();
            if n == 0 {
                self.formatter.finish(&mut self.output);
                self.done = true;
            } else {
                self.formatter.feed(&self.input[..n], &mut self.output);
                self.pos += n as u64;
                left = left.saturating_sub(n);
            }
            w.write_all(&self.output)?;
        }
        Ok(self.done)
    }
}

/// Formats the whole of `doc` into `w`.
pub fn format_document<W: Write + ?Sized>(
    doc: &Document,
    kind: Kind,
    indent: Indent,
    w: &mut W,
) -> io::Result<()> {
    let mut job = Format::new(doc, kind, indent);
    while !job.step(doc, 16 * CHUNK, w)? {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_first_byte_then_by_extension() {
        assert_eq!(detect(None, b"  \n{\"a\":1}"), Some(Kind::Json));
        assert_eq!(detect(None, b"[1]"), Some(Kind::Json));
        assert_eq!(
            detect(None, b"\xEF\xBB\xBF<r/>"),
            Some(Kind::Xml),
            "after a BOM"
        );
        let json = Path::new("data.JSON");
        assert_eq!(detect(Some(json), b"123"), Some(Kind::Json));
        assert_eq!(detect(Some(Path::new("feed.rss")), b""), Some(Kind::Xml));
        assert_eq!(detect(Some(Path::new("notes.txt")), b"hello"), None);
        assert_eq!(
            detect(Some(json), b"<r/>"),
            Some(Kind::Xml),
            "the content wins"
        );
    }

    #[test]
    fn a_stepped_format_matches_a_whole_one_and_sees_edits() {
        let mut doc = Document::from_text(r#"{"a":[1,2],"b":{"c":"d"}}"#);
        doc.index_complete().unwrap();
        doc.insert(1, r#""new":true,"#).unwrap();

        let mut whole = Vec::new();
        format_document(&doc, Kind::Json, Indent::default(), &mut whole).unwrap();
        assert_eq!(
            String::from_utf8(whole.clone()).unwrap(),
            "{\n  \"new\": true,\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {\n    \"c\": \"d\"\n  }\n}\n"
        );

        let mut stepped = Vec::new();
        let mut job = Format::new(&doc, Kind::Json, Indent::default());
        let mut steps = 0;
        while !job.step(&doc, 3, &mut stepped).unwrap() {
            steps += 1;
        }
        assert!(steps > 5, "a small budget takes many steps");
        assert_eq!(job.progress(), (doc.len(), doc.len()));
        assert_eq!(stepped, whole);
    }
}
