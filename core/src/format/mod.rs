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
//!
//! How the output is laid out — indent, line endings, final newline — is a
//! [`Style`], which a project sets through EditorConfig.

mod json;
mod xml;

pub use json::JsonFormatter;
pub use xml::XmlFormatter;

use crate::document::Document;
use crate::editorconfig::Properties;
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

/// How a line break is written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Newline {
    #[default]
    Lf,
    CrLf,
    Cr,
}

impl Newline {
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Newline::Lf => b"\n",
            Newline::CrLf => b"\r\n",
            Newline::Cr => b"\r",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Newline::Lf => "LF",
            Newline::CrLf => "CRLF",
            Newline::Cr => "CR",
        }
    }
}

/// How formatted output is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub indent: Indent,
    pub newline: Newline,
    /// Whether the output ends with a line break.
    pub final_newline: bool,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            indent: Indent::default(),
            newline: Newline::Lf,
            final_newline: true,
        }
    }
}

impl Style {
    /// The defaults, with what EditorConfig sets over them: `indent_style`,
    /// `indent_size` (a width, or `tab`), `tab_width`, `end_of_line` and
    /// `insert_final_newline`. A value the spec does not allow is ignored, as
    /// the spec asks.
    pub fn from_editorconfig(props: &Properties) -> Self {
        let mut style = Self::default();
        let width = |key: &str| {
            props
                .get(key)
                .and_then(|v| v.parse::<u8>().ok())
                .filter(|&n| n > 0)
        };
        let tab_width = width("tab_width");
        let size_is_tab = props
            .get("indent_size")
            .is_some_and(|v| v.eq_ignore_ascii_case("tab"));
        let size = if size_is_tab {
            tab_width
        } else {
            width("indent_size")
        };
        let indent_style = props.get("indent_style").map(str::to_ascii_lowercase);
        style.indent = match indent_style.as_deref() {
            Some("tab") => Indent::Tab,
            Some("space") => Indent::Spaces(size.or(tab_width).unwrap_or(2)),
            // No style, but the indent is to be a tab wide: indent with tabs.
            _ if size_is_tab => Indent::Tab,
            _ => size.map_or(style.indent, Indent::Spaces),
        };
        style.newline = match props
            .get("end_of_line")
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("lf") => Newline::Lf,
            Some("crlf") => Newline::CrLf,
            Some("cr") => Newline::Cr,
            _ => style.newline,
        };
        match props.get("insert_final_newline") {
            Some(v) if v.eq_ignore_ascii_case("true") => style.final_newline = true,
            Some(v) if v.eq_ignore_ascii_case("false") => style.final_newline = false,
            _ => {}
        }
        style
    }

    /// The style for a file at `path`, from the EditorConfig that applies
    /// there.
    pub fn for_path(path: &Path) -> Self {
        Self::from_editorconfig(&crate::editorconfig::properties_for(path))
    }

    /// In a few words, for a status line: `4 spaces, CRLF`.
    pub fn describe(&self) -> String {
        let indent = match self.indent {
            Indent::Tab => "tabs".to_string(),
            Indent::Spaces(1) => "1 space".to_string(),
            Indent::Spaces(n) => format!("{n} spaces"),
        };
        let end = if self.final_newline {
            ""
        } else {
            ", no final newline"
        };
        format!("{indent}, {}{end}", self.newline.name())
    }

    /// Starts a new line `depth` levels in.
    pub(crate) fn line(self, depth: usize, out: &mut Vec<u8>) {
        out.extend_from_slice(self.newline.bytes());
        let (byte, count) = match self.indent {
            Indent::Spaces(n) => (b' ', depth * n as usize),
            Indent::Tab => (b'\t', depth),
        };
        out.resize(out.len() + count, byte);
    }

    /// Ends output that has something in it.
    pub(crate) fn end(self, out: &mut Vec<u8>) {
        if self.final_newline {
            out.extend_from_slice(self.newline.bytes());
        }
    }
}

/// A push formatter: bytes in, formatted bytes out, a chunk at a time.
///
/// Feeding the same input in any chunking gives the same output.
pub trait Formatter {
    /// Formats `input`, appending to `out`.
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>);

    /// Ends the input, appending whatever was held back and, if the style
    /// asks for one, a final newline.
    fn finish(&mut self, out: &mut Vec<u8>);
}

/// A formatter for `kind`.
pub fn formatter(kind: Kind, style: Style) -> Box<dyn Formatter + Send> {
    match kind {
        Kind::Json => Box::new(JsonFormatter::new(style)),
        Kind::Xml => Box::new(XmlFormatter::new(style)),
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
    style: Style,
    pos: u64,
    len: u64,
    input: Vec<u8>,
    output: Vec<u8>,
    done: bool,
}

impl Format {
    pub fn new(doc: &Document, kind: Kind, style: Style) -> Self {
        Self {
            formatter: formatter(kind, style),
            kind,
            style,
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

    pub fn style(&self) -> Style {
        self.style
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
    style: Style,
    w: &mut W,
) -> io::Result<()> {
    let mut job = Format::new(doc, kind, style);
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
        format_document(&doc, Kind::Json, Style::default(), &mut whole).unwrap();
        assert_eq!(
            String::from_utf8(whole.clone()).unwrap(),
            "{\n  \"new\": true,\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {\n    \"c\": \"d\"\n  }\n}\n"
        );

        let mut stepped = Vec::new();
        let mut job = Format::new(&doc, Kind::Json, Style::default());
        let mut steps = 0;
        while !job.step(&doc, 3, &mut stepped).unwrap() {
            steps += 1;
        }
        assert!(steps > 5, "a small budget takes many steps");
        assert_eq!(job.progress(), (doc.len(), doc.len()));
        assert_eq!(stepped, whole);
    }

    fn style(pairs: &[(&str, &str)]) -> Style {
        Style::from_editorconfig(&pairs.iter().copied().collect())
    }

    #[test]
    fn editorconfig_sets_the_indent() {
        assert_eq!(style(&[]), Style::default());
        assert_eq!(style(&[("indent_style", "Tab")]).indent, Indent::Tab);
        assert_eq!(
            style(&[("indent_style", "space"), ("indent_size", "4")]).indent,
            Indent::Spaces(4)
        );
        assert_eq!(
            style(&[("indent_size", "3")]).indent,
            Indent::Spaces(3),
            "a size alone indents with spaces"
        );
        assert_eq!(
            style(&[("indent_style", "space"), ("tab_width", "8")]).indent,
            Indent::Spaces(8),
            "a missing size is the tab width"
        );
        assert_eq!(
            style(&[
                ("indent_style", "space"),
                ("indent_size", "tab"),
                ("tab_width", "6")
            ])
            .indent,
            Indent::Spaces(6)
        );
        assert_eq!(style(&[("indent_size", "tab")]).indent, Indent::Tab);
    }

    #[test]
    fn editorconfig_sets_line_endings_and_the_final_newline() {
        let s = style(&[("end_of_line", "CRLF"), ("insert_final_newline", "false")]);
        assert_eq!(s.newline, Newline::CrLf);
        assert!(!s.final_newline);
        assert_eq!(style(&[("end_of_line", "cr")]).newline, Newline::Cr);
        assert_eq!(s.describe(), "2 spaces, CRLF, no final newline");
        assert_eq!(style(&[("indent_style", "tab")]).describe(), "tabs, LF");
    }

    #[test]
    fn values_the_spec_does_not_allow_are_ignored() {
        assert_eq!(
            style(&[
                ("indent_style", "sideways"),
                ("indent_size", "wide"),
                ("tab_width", "0"),
                ("end_of_line", "nl"),
                ("insert_final_newline", "maybe"),
            ]),
            Style::default()
        );
        assert_eq!(style(&[("indent_size", "unset")]), Style::default());
    }

    #[test]
    fn a_whole_format_follows_the_style() {
        let doc = Document::from_text("<r><a>1</a></r>");
        let crlf_tabs = Style {
            indent: Indent::Tab,
            newline: Newline::CrLf,
            final_newline: false,
        };
        let mut out = Vec::new();
        format_document(&doc, Kind::Xml, crlf_tabs, &mut out).unwrap();
        assert_eq!(out, b"<r>\r\n\t<a>1</a>\r\n</r>");
    }
}
