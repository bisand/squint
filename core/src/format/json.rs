//! JSON, a byte at a time.
//!
//! Whitespace outside strings is dropped and laid out again: a newline and an
//! indent after every `{`, `[` and `,`, one before every `}` and `]`, and a
//! space after every `:`. An empty container stays on one line. Strings are
//! copied through in runs up to the next quote or backslash. Several values in
//! a row at the top level — JSON Lines, the usual shape of a big JSON log —
//! each start on a line of their own.
//!
//! State is a handful of flags and the depth, so memory does not grow with
//! the document however deep or long it is.

use super::{Formatter, Style};
use memchr::memchr2;

pub struct JsonFormatter {
    style: Style,
    depth: usize,
    in_string: bool,
    /// The last byte of the string was a backslash, whose escape has not
    /// been seen yet: it may be in the next chunk.
    escaped: bool,
    /// `{` or `[` was just written. The newline after it waits for the next
    /// token, so a container that closes straight away stays `{}`.
    opened: bool,
    /// Anything has been written. At the top level, the next value then
    /// starts a new line.
    wrote: bool,
    /// The last byte written outside a string belonged to a bare literal: a
    /// number, `true`, `false` or `null`.
    in_literal: bool,
    /// Whitespace has been skipped since the last byte written, which is what
    /// tells `1 2` from `12`.
    gap: bool,
}

impl JsonFormatter {
    pub fn new(style: Style) -> Self {
        Self {
            style,
            depth: 0,
            in_string: false,
            escaped: false,
            opened: false,
            wrote: false,
            in_literal: false,
            gap: false,
        }
    }

    /// Lays out the newline an opening bracket has been holding back.
    fn flush_open(&mut self, out: &mut Vec<u8>) {
        if self.opened {
            self.opened = false;
            self.style.line(self.depth, out);
        }
    }
}

impl Formatter for JsonFormatter {
    fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) {
        let mut i = 0;
        while i < input.len() {
            if self.in_string {
                if self.escaped {
                    out.push(input[i]);
                    self.escaped = false;
                    i += 1;
                    continue;
                }
                let rest = &input[i..];
                match memchr2(b'"', b'\\', rest) {
                    None => {
                        out.extend_from_slice(rest);
                        i = input.len();
                    }
                    Some(j) => {
                        out.extend_from_slice(&rest[..=j]);
                        i += j + 1;
                        if rest[j] == b'\\' {
                            self.escaped = true;
                        } else {
                            self.in_string = false;
                        }
                    }
                }
                continue;
            }

            let b = input[i];
            i += 1;
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.gap = true;
                continue;
            }
            let literal = !matches!(b, b'{' | b'}' | b'[' | b']' | b',' | b':' | b'"');
            // Punctuation and the rest of a literal carry on the value being
            // written; anything else at the top level starts the next one.
            let carries_on =
                matches!(b, b',' | b':' | b'}' | b']') || (literal && self.in_literal && !self.gap);
            if self.depth == 0 && self.wrote && !carries_on {
                out.extend_from_slice(self.style.newline.bytes());
            }
            match b {
                b'{' | b'[' => {
                    self.flush_open(out);
                    out.push(b);
                    self.depth += 1;
                    self.opened = true;
                }
                b'}' | b']' => {
                    self.depth = self.depth.saturating_sub(1);
                    if self.opened {
                        self.opened = false;
                    } else {
                        self.style.line(self.depth, out);
                    }
                    out.push(b);
                }
                b',' => {
                    self.flush_open(out);
                    out.push(b);
                    if self.depth > 0 {
                        self.style.line(self.depth, out);
                    }
                }
                b':' => {
                    self.flush_open(out);
                    out.extend_from_slice(b": ");
                }
                b'"' => {
                    self.flush_open(out);
                    out.push(b);
                    self.in_string = true;
                }
                _ => {
                    self.flush_open(out);
                    out.push(b);
                }
            }
            self.in_literal = literal;
            self.gap = false;
            self.wrote = true;
        }
    }

    fn finish(&mut self, out: &mut Vec<u8>) {
        if self.wrote {
            self.style.end(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{Indent, Newline};

    fn pretty_with(input: &str, style: Style) -> String {
        let mut f = JsonFormatter::new(style);
        let mut out = Vec::new();
        f.feed(input.as_bytes(), &mut out);
        f.finish(&mut out);
        String::from_utf8(out).unwrap()
    }

    fn pretty(input: &str) -> String {
        pretty_with(input, Style::default())
    }

    /// The same input fed a byte at a time.
    fn pretty_bytewise(input: &str) -> String {
        let mut f = JsonFormatter::new(Style::default());
        let mut out = Vec::new();
        for b in input.as_bytes() {
            f.feed(std::slice::from_ref(b), &mut out);
        }
        f.finish(&mut out);
        String::from_utf8(out).unwrap()
    }

    const NESTED: &str = r#"{"a":[1,2,{}],"b":"x\"y","c":{"d":null}}"#;

    #[test]
    fn nests_and_indents() {
        assert_eq!(
            pretty(NESTED),
            r#"{
  "a": [
    1,
    2,
    {}
  ],
  "b": "x\"y",
  "c": {
    "d": null
  }
}
"#
        );
    }

    #[test]
    fn empty_containers_stay_on_one_line() {
        assert_eq!(
            pretty(r#"{"a":{},"b":[ ]}"#),
            "{\n  \"a\": {},\n  \"b\": []\n}\n"
        );
    }

    #[test]
    fn strings_are_copied_byte_for_byte() {
        assert_eq!(
            pretty(r#"{"s":"a,b:{c}[d] \"e\" \\","t":"æøå"}"#),
            "{\n  \"s\": \"a,b:{c}[d] \\\"e\\\" \\\\\",\n  \"t\": \"æøå\"\n}\n"
        );
    }

    #[test]
    fn json_lines_come_out_one_record_after_another() {
        assert_eq!(
            pretty("{\"a\":1}\n{\"b\":2}\n"),
            "{\n  \"a\": 1\n}\n{\n  \"b\": 2\n}\n"
        );
        assert_eq!(pretty("1 2"), "1\n2\n", "two literals");
        assert_eq!(pretty("12"), "12\n", "one literal");
        assert_eq!(pretty("\"a\"\"b\""), "\"a\"\n\"b\"\n", "two strings");
        assert_eq!(pretty("  "), "", "nothing in, nothing out");
    }

    #[test]
    fn tabs_indent_one_per_level() {
        assert_eq!(
            pretty_with(
                r#"{"a":[1]}"#,
                Style {
                    indent: Indent::Tab,
                    ..Style::default()
                }
            ),
            "{\n\t\"a\": [\n\t\t1\n\t]\n}\n"
        );
    }

    #[test]
    fn line_endings_and_the_final_newline_follow_the_style() {
        let style = Style {
            newline: Newline::CrLf,
            final_newline: false,
            ..Style::default()
        };
        assert_eq!(
            pretty_with("[1,{}]\n{\"a\":2}", style),
            "[\r\n  1,\r\n  {}\r\n]\r\n{\r\n  \"a\": 2\r\n}",
            "between records too, and nothing after the last"
        );
    }

    #[test]
    fn any_chunking_gives_the_same_output() {
        let escape_at_a_boundary = r#"{"k":"\\\"","l":[true,false]}"#;
        for input in [NESTED, escape_at_a_boundary, "{\"a\":1}\n[2,3]"] {
            assert_eq!(pretty_bytewise(input), pretty(input), "{input}");
        }
    }

    #[test]
    fn formatting_twice_changes_nothing() {
        let once = pretty(NESTED);
        assert_eq!(pretty(&once), once);
    }
}
