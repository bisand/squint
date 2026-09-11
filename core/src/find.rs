//! Finding text in a document without loading it.
//!
//! A [`Find`] walks the document in windows, a bounded number of bytes per
//! [`Find::step`], so a front end can search a multi-gigabyte file between
//! frames the way it builds the line index: the window keeps drawing, and the
//! search says how far it has got. Each window reads one byte less than the
//! needle past its end, so a match that straddles two windows is still found
//! in the first.
//!
//! Matching is on bytes. A query with no upper-case letter ignores ASCII case
//! ("smart case"); letters outside ASCII always match exactly as typed.

use crate::document::Document;
use memchr::memmem;
use std::io;
use std::ops::Range;

/// The most bytes one window covers, whatever the budget.
const WINDOW: u64 = 1024 * 1024;

/// What one [`Find::step`] came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindStep {
    /// A match starts at `at`. `wrapped` if the search went past the end of
    /// the document (backwards: past its start) to reach it.
    Found { at: u64, wrapped: bool },
    /// The budget ran out before an answer; call again.
    Pending,
    /// No match anywhere in the document.
    NotFound,
}

/// A query, as it is matched.
///
/// Shared by [`Find`], which walks a document with it, and by anything that
/// marks matches in text already read — the lines on screen — so the two can
/// never disagree about what counts as one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Needle {
    /// The query's bytes, lower-cased when it ignores case.
    bytes: Vec<u8>,
    case_sensitive: bool,
}

impl Needle {
    /// `query` as matched: exactly if it has an upper-case letter, else
    /// ignoring ASCII case.
    pub fn new(query: &str) -> Self {
        let case_sensitive = query.chars().any(char::is_uppercase);
        let bytes = if case_sensitive {
            query.as_bytes().to_vec()
        } else {
            query.to_ascii_lowercase().into_bytes()
        };
        Self {
            bytes,
            case_sensitive,
        }
    }

    /// Its length in bytes, which is also the length of every match.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Whether case must match exactly: the query has an upper-case letter.
    pub fn is_case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    /// Appends every match in `hay` to `out`, as byte ranges, in order and
    /// without overlaps.
    pub fn matches_in(&self, hay: &[u8], out: &mut Vec<Range<usize>>) {
        let n = self.bytes.len();
        if n == 0 || hay.len() < n {
            return;
        }
        let lowered;
        let hay = if self.case_sensitive {
            hay
        } else {
            lowered = hay.to_ascii_lowercase();
            &lowered
        };
        out.extend(memmem::find_iter(hay, &self.bytes).map(|i| i..i + n));
    }
}

/// A search in progress.
///
/// Built for the document as it is: an edit moves the offsets it has
/// covered, so a front end starts a new one after changing the text.
pub struct Find {
    needle: Needle,
    forward: bool,
    /// Ranges of match *starts* to examine, in order: from the origin to the
    /// end and then the part before it (backwards, the other way round).
    segments: [(u64, u64); 2],
    seg: usize,
    /// Forwards, the next start to examine; backwards, the exclusive end of
    /// the starts not yet examined.
    cursor: u64,
    covered: u64,
    len: u64,
}

impl Find {
    /// A search for `query`: forwards for the first match starting at or
    /// after `from`, or backwards for the last one starting before it.
    pub fn new(doc: &Document, query: &str, from: u64, forward: bool) -> Self {
        let needle = Needle::new(query);
        let len = doc.len();
        let from = from.min(len);
        let segments = if forward {
            [(from, len), (0, from)]
        } else {
            [(0, from), (from, len)]
        };
        let cursor = if forward { segments[0].0 } else { segments[0].1 };
        Self {
            needle,
            forward,
            segments,
            seg: 0,
            cursor,
            covered: 0,
            len,
        }
    }

    /// Whether the query matches case exactly: it has an upper-case letter.
    pub fn is_case_sensitive(&self) -> bool {
        self.needle.is_case_sensitive()
    }

    /// Bytes examined so far, out of the document's length.
    pub fn progress(&self) -> (u64, u64) {
        (self.covered.min(self.len), self.len)
    }

    /// Examines up to `budget` more match starts.
    pub fn step(&mut self, doc: &Document, budget: usize) -> io::Result<FindStep> {
        let n = self.needle.len() as u64;
        if n == 0 || n > self.len {
            return Ok(FindStep::NotFound);
        }
        let mut left = (budget as u64).max(1);
        let mut buf = Vec::new();
        while self.seg < 2 {
            let (lo, hi) = self.segments[self.seg];
            let exhausted = if self.forward {
                self.cursor >= hi
            } else {
                self.cursor <= lo
            };
            if exhausted {
                self.seg += 1;
                if self.seg < 2 {
                    let (lo, hi) = self.segments[1];
                    self.cursor = if self.forward { lo } else { hi };
                }
                continue;
            }
            if left == 0 {
                return Ok(FindStep::Pending);
            }
            let span = left.min(WINDOW);
            let (a, b) = if self.forward {
                (self.cursor, (self.cursor + span).min(hi))
            } else {
                (self.cursor.saturating_sub(span).max(lo), self.cursor)
            };
            // Bytes for every start in [a, b): the last start needs n - 1
            // more after it. A match in these bytes therefore starts in the
            // window, never past it.
            let end = (b + n - 1).min(self.len);
            buf.resize((end - a) as usize, 0);
            let got = doc.read_at(a, &mut buf)?;
            buf.truncate(got);
            if !self.needle.case_sensitive {
                buf.make_ascii_lowercase();
            }
            let hit = if self.forward {
                memmem::find(&buf, &self.needle.bytes)
            } else {
                memmem::rfind(&buf, &self.needle.bytes)
            };
            if let Some(i) = hit {
                return Ok(FindStep::Found {
                    at: a + i as u64,
                    wrapped: self.seg == 1,
                });
            }
            self.covered += b - a;
            left = left.saturating_sub(b - a);
            self.cursor = if self.forward { b } else { a };
        }
        Ok(FindStep::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(doc: &Document, query: &str, from: u64, forward: bool, budget: usize) -> FindStep {
        let mut find = Find::new(doc, query, from, forward);
        loop {
            match find.step(doc, budget).unwrap() {
                FindStep::Pending => continue,
                done => return done,
            }
        }
    }

    fn found(at: u64, wrapped: bool) -> FindStep {
        FindStep::Found { at, wrapped }
    }

    #[test]
    fn forwards_finds_the_first_match_at_or_after_the_origin() {
        let d = Document::from_text("cat dog cat dog");
        assert_eq!(run(&d, "dog", 0, true, 100), found(4, false));
        assert_eq!(run(&d, "dog", 4, true, 100), found(4, false), "at the origin counts");
        assert_eq!(run(&d, "dog", 5, true, 100), found(12, false));
        assert_eq!(run(&d, "dog", 13, true, 100), found(4, true), "past the end wraps");
        assert_eq!(run(&d, "bird", 3, true, 100), FindStep::NotFound);
    }

    #[test]
    fn backwards_finds_the_last_match_before_the_origin() {
        let d = Document::from_text("cat dog cat dog");
        assert_eq!(run(&d, "cat", 15, false, 100), found(8, false));
        assert_eq!(run(&d, "cat", 8, false, 100), found(0, false), "the origin does not count");
        assert_eq!(run(&d, "cat", 0, false, 100), found(8, true), "past the start wraps");
        assert_eq!(run(&d, "bird", 9, false, 100), FindStep::NotFound);
    }

    #[test]
    fn a_match_across_a_window_boundary_is_found() {
        let text: String = "x".repeat(50) + "needle" + &"y".repeat(50);
        let d = Document::from_text(&text);
        for budget in 1..12 {
            assert_eq!(run(&d, "needle", 0, true, budget), found(50, false), "budget {budget}");
            assert_eq!(run(&d, "needle", 106, false, budget), found(50, false), "budget {budget}");
        }
    }

    #[test]
    fn a_small_budget_pends_and_reports_progress() {
        let text = "a".repeat(1000) + "b";
        let d = Document::from_text(&text);
        let mut find = Find::new(&d, "b", 0, true);
        assert_eq!(find.step(&d, 100).unwrap(), FindStep::Pending);
        assert_eq!(find.progress(), (100, 1001));
        let mut steps = 1;
        while find.step(&d, 100).unwrap() == FindStep::Pending {
            steps += 1;
        }
        assert_eq!(steps, 10);
    }

    #[test]
    fn lower_case_queries_ignore_ascii_case() {
        let d = Document::from_text("Hello HELLO hello");
        assert!(!Find::new(&d, "hello", 0, true).is_case_sensitive());
        assert_eq!(run(&d, "hello", 1, true, 100), found(6, false));
        assert!(Find::new(&d, "Hello", 0, true).is_case_sensitive());
        assert_eq!(run(&d, "Hello", 1, true, 100), found(0, true));
        assert_eq!(run(&d, "HELLO", 7, true, 100), found(6, true));
    }

    #[test]
    fn a_needle_marks_every_match_in_a_line_once() {
        let mut out = Vec::new();
        Needle::new("aa").matches_in(b"aaaaa", &mut out);
        assert_eq!(out, vec![0..2, 2..4], "matches do not overlap");
        out.clear();
        Needle::new("error").matches_in(b"ERROR: an Error, error", &mut out);
        assert_eq!(out, vec![0..5, 10..15, 17..22]);
        out.clear();
        Needle::new("Error").matches_in(b"ERROR: an Error, error", &mut out);
        assert_eq!(out, vec![10..15], "a capital asks for exact case");
        out.clear();
        Needle::new("").matches_in(b"anything", &mut out);
        Needle::new("longer than this").matches_in(b"short", &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn finds_text_that_was_typed_in() {
        let mut d = Document::from_text("one\ntwo\nthree");
        d.index_complete().unwrap();
        d.insert(4, "inserted ").unwrap();
        // "one\ninserted two\nthree": the match runs from the added text
        // into the original, starting at the `t` of "inserted".
        assert_eq!(run(&d, "ted two", 0, true, 3), found(9, false));
        assert_eq!(run(&d, "", 0, true, 3), FindStep::NotFound, "nothing to find");
        assert_eq!(run(&d, &"z".repeat(100), 0, true, 3), FindStep::NotFound, "longer than the text");
    }
}
