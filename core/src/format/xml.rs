//! XML, a byte at a time.
//!
//! What it changes: text that is only whitespace between markup is dropped,
//! and every tag, comment, processing instruction and declaration goes on a
//! line of its own, indented by depth. An element with nothing in it stays
//! `<a></a>`, and one holding only text stays `<a>text</a>`.
//!
//! What it leaves alone: every byte of every token, and all text that is not
//! only whitespace. An element holding text is mixed content, where
//! whitespace can matter, so from its first text on — and in every element
//! inside it — nothing is added or dropped. The same holds for an element
//! marked `xml:space="preserve"`. A stream cannot look ahead, so markup that
//! comes before the first text of a mixed element has already been laid out.
//!
//! Memory is a flag per open element and two small buffers: the text since
//! the last markup, capped, and the start of a tag, capped, for finding
//! `xml:space`.

use super::{Formatter, Indent};
use memchr::{memchr, memmem};

/// Text held back to decide whether it is only whitespace. Past this it is
/// written as it is.
const TEXT_CAP: usize = 64 * 1024;

/// How much of a start tag is kept to look for `xml:space` in.
const TAG_KEEP: usize = 4096;

/// What was written last, as far as layout cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prev {
    Nothing,
    Start,
    End,
    Empty,
    Text,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Text,
    /// After `<`, until enough has been seen to know what it opens.
    Open,
    Tag {
        end: bool,
    },
    Comment,
    CData,
    Pi,
    Decl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    Start,
    End,
    Comment,
    CData,
    Pi,
    Decl,
}

pub struct XmlFormatter {
    indent: Indent,
    state: State,
    /// The bytes from `<` while the markup is being told apart; at most nine.
    open: Vec<u8>,
    text: Vec<u8>,
    /// For each open element, whether layout is off inside it: it is mixed
    /// content, preserves space, or is inside one that does.
    stack: Vec<bool>,
    prev: Prev,
    wrote: bool,
    /// The quote an attribute value is open in, or 0.
    quote: u8,
    /// The last byte of the tag that was not whitespace was `/`.
    slash: bool,
    tag: Vec<u8>,
    /// The two bytes before this one, for spotting `-->`, `]]>` and `?>`.
    tail: [u8; 2],
    /// `[` nesting in a declaration's internal subset.
    brackets: usize,
}

impl XmlFormatter {
    pub fn new(indent: Indent) -> Self {
        Self {
            indent,
            state: State::Text,
            open: Vec::with_capacity(9),
            text: Vec::new(),
            stack: Vec::new(),
            prev: Prev::Nothing,
            wrote: false,
            quote: 0,
            slash: false,
            tag: Vec::new(),
            tail: [0; 2],
            brackets: 0,
        }
    }

    /// Layout is off in the element being written into.
    fn inline(&self) -> bool {
        self.stack.last().copied().unwrap_or(false)
    }

    /// The element being written into holds text: layout goes off in it.
    fn mark_mixed(&mut self) {
        if let Some(top) = self.stack.last_mut() {
            *top = true;
        }
    }

    /// A new line `depth` levels in, unless nothing has been written yet.
    fn line(&mut self, depth: usize, out: &mut Vec<u8>) {
        if self.wrote {
            self.indent.newline(depth, out);
        }
    }

    fn take_text(&mut self, mut bytes: &[u8], out: &mut Vec<u8>) {
        while !bytes.is_empty() {
            let n = (TEXT_CAP - self.text.len()).min(bytes.len());
            self.text.extend_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if self.text.len() == TEXT_CAP {
                self.flush_text(out);
            }
        }
    }

    fn flush_text(&mut self, out: &mut Vec<u8>) {
        if self.text.is_empty() {
            return;
        }
        if self.text.iter().all(|&b| is_space(b)) {
            // Whitespace between markup is what gets laid out again, except
            // where layout is off and it may be content.
            if self.inline() {
                out.extend_from_slice(&self.text);
            }
        } else {
            out.extend_from_slice(&self.text);
            self.mark_mixed();
            self.prev = Prev::Text;
            self.wrote = true;
        }
        self.text.clear();
    }

    fn start_markup(&mut self, out: &mut Vec<u8>) {
        self.flush_text(out);
        self.state = State::Open;
        self.open.clear();
        self.open.push(b'<');
    }

    /// Decides what `open` holds once it can, lays it out and replays it.
    fn classify(&mut self, out: &mut Vec<u8>) {
        let decided = {
            let o = &self.open;
            if o.len() < 2 {
                return;
            }
            match o[1] {
                b'/' => (Token::End, 2),
                b'?' => (Token::Pi, 2),
                b'!' if o.starts_with(b"<!--") => (Token::Comment, 4),
                b'!' if o.starts_with(b"<![CDATA[") => (Token::CData, 9),
                b'!' if b"<!--".starts_with(o) || b"<![CDATA[".starts_with(o) => return,
                b'!' => (Token::Decl, 2),
                _ => (Token::Start, 1),
            }
        };
        let (token, prefix) = decided;
        let mut held = [0u8; 9];
        let n = self.open.len();
        held[..n].copy_from_slice(&self.open);
        self.open.clear();
        self.begin(token, out);
        out.extend_from_slice(&held[..prefix]);
        if token == Token::Start {
            self.tag.extend_from_slice(&held[..prefix]);
        }
        for &b in &held[prefix..n] {
            self.byte(b, out);
        }
    }

    /// Lays out the start of a token and enters the state that reads it.
    fn begin(&mut self, token: Token, out: &mut Vec<u8>) {
        let inline = self.inline();
        match token {
            Token::End => {
                if self.prev != Prev::Start && !inline {
                    self.line(self.stack.len().saturating_sub(1), out);
                }
            }
            // Character data is content, like text: nothing goes around it.
            Token::CData => self.mark_mixed(),
            _ => {
                if !inline {
                    self.line(self.stack.len(), out);
                }
            }
        }
        self.state = match token {
            Token::Start => State::Tag { end: false },
            Token::End => State::Tag { end: true },
            Token::Comment => State::Comment,
            Token::CData => State::CData,
            Token::Pi => State::Pi,
            Token::Decl => State::Decl,
        };
        self.wrote = true;
        self.quote = 0;
        self.slash = false;
        self.tag.clear();
        self.tail = [0; 2];
        self.brackets = 0;
    }

    fn close_tag(&mut self, end: bool) {
        self.state = State::Text;
        if end {
            self.stack.pop();
            self.prev = Prev::End;
        } else if self.slash {
            self.prev = Prev::Empty;
        } else {
            let inherited = self.inline();
            self.stack.push(inherited || preserves_space(&self.tag));
            self.prev = Prev::Start;
        }
    }

    fn close(&mut self, prev: Prev) {
        self.state = State::Text;
        self.prev = prev;
    }

    fn shift(&mut self, b: u8) {
        self.tail = [self.tail[1], b];
    }

    /// One byte of markup, or of text reached while replaying markup.
    fn byte(&mut self, b: u8, out: &mut Vec<u8>) {
        match self.state {
            State::Text => {
                if b == b'<' {
                    self.start_markup(out);
                } else {
                    self.take_text(&[b], out);
                }
            }
            State::Open => {
                self.open.push(b);
                self.classify(out);
            }
            State::Tag { end } => {
                out.push(b);
                if !end && self.tag.len() < TAG_KEEP {
                    self.tag.push(b);
                }
                if self.quote != 0 {
                    if b == self.quote {
                        self.quote = 0;
                    }
                    return;
                }
                match b {
                    b'"' | b'\'' => {
                        self.quote = b;
                        self.slash = false;
                    }
                    b'>' => self.close_tag(end),
                    b'/' => self.slash = true,
                    b if is_space(b) => {}
                    _ => self.slash = false,
                }
            }
            State::Comment => {
                out.push(b);
                if b == b'>' && self.tail == *b"--" {
                    self.close(Prev::Other);
                } else {
                    self.shift(b);
                }
            }
            State::CData => {
                out.push(b);
                if b == b'>' && self.tail == *b"]]" {
                    self.close(Prev::Text);
                } else {
                    self.shift(b);
                }
            }
            State::Pi => {
                out.push(b);
                if b == b'>' && self.tail[1] == b'?' {
                    self.close(Prev::Other);
                } else {
                    self.shift(b);
                }
            }
            State::Decl => {
                out.push(b);
                if self.quote != 0 {
                    if b == self.quote {
                        self.quote = 0;
                    }
                    return;
                }
                match b {
                    b'"' | b'\'' => self.quote = b,
                    b'[' => self.brackets += 1,
                    b']' => self.brackets = self.brackets.saturating_sub(1),
                    b'>' if self.brackets == 0 => self.close(Prev::Other),
                    _ => {}
                }
            }
        }
    }
}

impl Formatter for XmlFormatter {
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) {
        let mut i = 0;
        while i < input.len() {
            if self.state == State::Text {
                // Text runs to the next `<` in one go.
                let rest = &input[i..];
                let end = memchr(b'<', rest).unwrap_or(rest.len());
                self.take_text(&rest[..end], out);
                i += end;
                if i < input.len() {
                    i += 1;
                    self.start_markup(out);
                }
                continue;
            }
            let b = input[i];
            i += 1;
            self.byte(b, out);
        }
    }

    fn finish(&mut self, out: &mut Vec<u8>) {
        self.flush_text(out);
        if self.state == State::Open {
            // A `<` the input ended on: written as it came.
            out.extend_from_slice(&self.open);
            self.open.clear();
            self.wrote = true;
        }
        if self.wrote {
            out.push(b'\n');
        }
    }
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Whether a start tag says `xml:space="preserve"`.
fn preserves_space(tag: &[u8]) -> bool {
    let Some(at) = memmem::find(tag, b"xml:space") else {
        return false;
    };
    let rest = skip_space(&tag[at + b"xml:space".len()..]);
    let Some(rest) = rest.strip_prefix(b"=") else {
        return false;
    };
    let rest = skip_space(rest);
    rest.starts_with(b"\"preserve\"") || rest.starts_with(b"'preserve'")
}

fn skip_space(bytes: &[u8]) -> &[u8] {
    let n = bytes.iter().take_while(|&&b| is_space(b)).count();
    &bytes[n..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pretty(input: &str) -> String {
        let mut f = XmlFormatter::new(Indent::default());
        let mut out = Vec::new();
        f.feed(input.as_bytes(), &mut out);
        f.finish(&mut out);
        String::from_utf8(out).unwrap()
    }

    fn pretty_bytewise(input: &str) -> String {
        let mut f = XmlFormatter::new(Indent::default());
        let mut out = Vec::new();
        for b in input.as_bytes() {
            f.feed(std::slice::from_ref(b), &mut out);
        }
        f.finish(&mut out);
        String::from_utf8(out).unwrap()
    }

    const DOCUMENT: &str =
        r#"<?xml version="1.0"?><root><a x="1">text</a><b/><c><d></d></c><!-- note --></root>"#;

    #[test]
    fn lays_out_elements_by_depth() {
        assert_eq!(
            pretty(DOCUMENT),
            r#"<?xml version="1.0"?>
<root>
  <a x="1">text</a>
  <b/>
  <c>
    <d></d>
  </c>
  <!-- note -->
</root>
"#
        );
    }

    #[test]
    fn a_gt_inside_an_attribute_or_a_comment_ends_nothing() {
        assert_eq!(
            pretty(r#"<a title="x > y"><b/></a>"#),
            "<a title=\"x > y\">\n  <b/>\n</a>\n"
        );
        assert_eq!(
            pretty("<r><!-- a > b --><x/></r>"),
            "<r>\n  <!-- a > b -->\n  <x/>\n</r>\n"
        );
        assert_eq!(
            pretty("<s><![CDATA[a<b>c]]></s>"),
            "<s><![CDATA[a<b>c]]></s>\n",
            "character data is content"
        );
    }

    #[test]
    fn mixed_content_is_left_as_it_is() {
        let p = "<p>Hello <b>big</b> world</p>";
        assert_eq!(pretty(p), format!("{p}\n"));
        assert_eq!(
            pretty("<doc><p>An <i>inline <b>nested</b></i> word</p><q/></doc>"),
            "<doc>\n  <p>An <i>inline <b>nested</b></i> word</p>\n  <q/>\n</doc>\n"
        );
    }

    #[test]
    fn preserved_space_is_preserved() {
        assert_eq!(
            pretty("<r><pre xml:space=\"preserve\">  <x/>\n</pre></r>"),
            "<r>\n  <pre xml:space=\"preserve\">  <x/>\n</pre>\n</r>\n"
        );
        assert_eq!(
            pretty("<r><pre xml:space = 'preserve'> <x/> </pre></r>"),
            "<r>\n  <pre xml:space = 'preserve'> <x/> </pre>\n</r>\n"
        );
    }

    #[test]
    fn a_doctype_with_an_internal_subset_is_one_token() {
        assert_eq!(
            pretty(r#"<!DOCTYPE r [<!ENTITY e "v">]><r/>"#),
            "<!DOCTYPE r [<!ENTITY e \"v\">]>\n<r/>\n"
        );
    }

    #[test]
    fn existing_indentation_is_replaced() {
        assert_eq!(
            pretty("<root>\n\t\t<a>1</a>\n    <b>\n</b>\n</root>\n"),
            "<root>\n  <a>1</a>\n  <b></b>\n</root>\n"
        );
    }

    #[test]
    fn long_text_is_written_through_past_the_cap() {
        let body = "x".repeat(3 * TEXT_CAP + 17);
        let input = format!("<r><a>{body}</a></r>");
        assert_eq!(pretty(&input), format!("<r>\n  <a>{body}</a>\n</r>\n"));
    }

    #[test]
    fn any_chunking_gives_the_same_output() {
        for input in [
            DOCUMENT,
            r#"<a title="x > y"><b/></a>"#,
            "<s><![CDATA[a<b>c]]><!-- - -- --></s>",
            r#"<!DOCTYPE r [<!ENTITY e "v">]><r><?pi a?b ?></r>"#,
            "<r><pre xml:space=\"preserve\">  <x/>\n</pre></r>",
        ] {
            assert_eq!(pretty_bytewise(input), pretty(input), "{input}");
        }
    }

    #[test]
    fn formatting_twice_changes_nothing() {
        for input in [DOCUMENT, "<doc><p>An <i>x</i> y</p><q/></doc>"] {
            let once = pretty(input);
            assert_eq!(pretty(&once), once, "{input}");
        }
    }
}
