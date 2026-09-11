//! Syntax highlighting with syntect, over a document that is never loaded.
//!
//! A grammar carries state from line to line — a comment or a string opened
//! on one line colours the next — so a line's colours depend on every line
//! above it. For a file squint opens that could be millions of lines, so the
//! state is handled like the line index handles offsets:
//!
//! - **From the top, in the background.** [`Syntax::advance`] parses on a few
//!   milliseconds at a time between frames and keeps the state at the start
//!   of every [`STRIDE`]-th line. A line the parse has passed is coloured
//!   exactly, from the nearest snapshot above it.
//! - **Guessed until then.** A line the parse has not reached is parsed from
//!   [`LOOKBACK`] lines above it with the state begun afresh. That is right
//!   unless a comment or string opened further up than that, and it is put
//!   right when the parse from the top arrives.
//! - **Only guessed, past a size.** A file bigger than [`BACKGROUND_LIMIT`] is
//!   not parsed from the top at all; what is on screen is parsed, and nothing
//!   else.
//!
//! The grammars and theme are syntect's defaults — the Sublime Text packages
//! `bat` also uses — and take tens of milliseconds and a few megabytes to
//! load, so they load on a thread, and only for a file that has a grammar.

use crate::document::Document;
use crate::format;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use syntect::highlighting::{
    Color, HighlightIterator, HighlightState, Highlighter, Theme, ThemeSet,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// Lines between the snapshots the parse from the top keeps.
pub const STRIDE: u64 = 512;

/// How far above a line the parse for it starts, when nothing better is known.
pub const LOOKBACK: u64 = 256;

/// Lines parsed past the one asked for, so the rest of a screen is ready.
const PREFETCH: u64 = 96;

/// A longer line is not parsed: it is left uncoloured and the state is carried
/// across it unchanged. Minified documents are one such line, and syntect on
/// megabytes of one line would stall a frame.
pub const MAX_LINE: usize = 16 * 1024;

/// Past this many bytes, a document is never parsed from the top.
///
/// syntect parses formatted JSON at about 5 MB a second, so this bounds the
/// background work at a few seconds. What the parse from the top buys is
/// exactness across constructs that span lines — block comments, long
/// strings — which live in source files far smaller than this; the big files
/// squint is for are logs, JSON and XML, where a parse begun a little above
/// the screen is right.
pub const BACKGROUND_LIMIT: u64 = 16 * 1024 * 1024;

/// Coloured lines kept; past this the cache starts again.
const CACHE_LIMIT: usize = 8192;

/// The theme, chosen to sit on squint's dark background.
const THEME: &str = "base16-ocean.dark";

struct Assets {
    syntaxes: SyntaxSet,
    theme: Theme,
}

static ASSETS: OnceLock<Assets> = OnceLock::new();
static LOADING: AtomicBool = AtomicBool::new(false);

/// Loads the grammars and the theme if they are not loaded yet, and waits
/// for them. Where a frame is waiting, use [`load_in_background`].
pub fn load() {
    ASSETS.get_or_init(|| {
        let mut themes = ThemeSet::load_defaults();
        Assets {
            syntaxes: SyntaxSet::load_defaults_newlines(),
            theme: themes.themes.remove(THEME).unwrap_or_default(),
        }
    });
}

/// Starts [`load`] on a thread of its own, once.
pub fn load_in_background() {
    if !LOADING.swap(true, Ordering::SeqCst) {
        std::thread::spawn(load);
    }
}

pub fn is_loaded() -> bool {
    ASSETS.get().is_some()
}

fn assets() -> &'static Assets {
    load();
    ASSETS.get().expect("loaded")
}

/// Whether a file is worth loading the grammars for. Logs and plain text are
/// what squint opens most, and they never pay for syntect: no grammar would
/// colour them. A file with no extension is worth it only when its first
/// bytes say what it is.
pub fn worth_loading(path: Option<&Path>, head: &[u8]) -> bool {
    let ext = path
        .and_then(Path::extension)
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("log" | "txt" | "text" | "out" | "csv" | "tsv") => false,
        Some(_) => true,
        None => head.starts_with(b"#!") || format::detect(None, head).is_some(),
    }
}

/// A coloured run of a line: bytes `start..end` in `rgb`.
///
/// Only text coloured differently from the theme's default foreground gets a
/// run, so the rest is drawn in whatever colour the front end draws text in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub start: usize,
    pub end: usize,
    pub rgb: [u8; 3],
}

/// The parse state at the start of a line.
#[derive(Clone)]
struct State {
    parse: ParseState,
    high: HighlightState,
}

impl State {
    fn new(syntax: &SyntaxReference, highlighter: &Highlighter) -> Self {
        Self {
            parse: ParseState::new(syntax),
            high: HighlightState::new(highlighter, ScopeStack::new()),
        }
    }

    /// Carries the state across one line, appending the line's runs to `out`
    /// if there is one. `None` is a line too long to parse: it gets no runs
    /// and the state goes on as it was.
    fn feed(
        &mut self,
        text: Option<&str>,
        highlighter: &Highlighter,
        assets: &Assets,
        scratch: &mut String,
        mut out: Option<&mut Vec<Run>>,
    ) {
        let Some(text) = text else {
            return;
        };
        // The grammars are the ones that expect each line to end in `\n`.
        scratch.clear();
        scratch.push_str(text);
        scratch.push('\n');
        let Ok(ops) = self.parse.parse_line(scratch, &assets.syntaxes) else {
            return;
        };
        let plain = assets.theme.settings.foreground.unwrap_or(Color::WHITE);
        let mut at = 0;
        for (style, piece) in HighlightIterator::new(&mut self.high, &ops, scratch, highlighter) {
            let end = (at + piece.len()).min(text.len());
            if let Some(runs) = out.as_deref_mut()
                && at < end
                && style.foreground != plain
            {
                let fg = style.foreground;
                let rgb = [fg.r, fg.g, fg.b];
                match runs.last_mut() {
                    Some(last) if last.end == at && last.rgb == rgb => last.end = end,
                    _ => runs.push(Run {
                        start: at,
                        end,
                        rgb,
                    }),
                }
            }
            at += piece.len();
        }
    }
}

struct Cached {
    runs: Vec<Run>,
    /// Parsed from a state known to be right, rather than guessed.
    exact: bool,
}

/// Highlighting for one document.
pub struct Syntax {
    syntax: &'static SyntaxReference,
    /// The state at the start of line `k * STRIDE`, for each `k` the parse
    /// from the top has passed.
    snapshots: Vec<State>,
    /// How far the parse from the top has got: the state at the start of
    /// this line, with every line above it parsed.
    frontier: (u64, State),
    /// The parse from the top is finished, or not wanted.
    settled: bool,
    /// Where the last parse for a screen stopped, to carry on from when the
    /// next lines asked for are just below it: the line, the state at its
    /// start, and whether that state is exact.
    cursor: Option<(u64, State, bool)>,
    cache: HashMap<u64, Cached>,
}

impl Syntax {
    /// Highlighting for a file with this path and these first bytes, or
    /// `None` when the grammars are not loaded yet or none suits it. A grammar
    /// is picked by the file's name, its extension, its first line, and last
    /// by whether it looks like JSON or XML.
    pub fn detect(path: Option<&Path>, head: &[u8]) -> Option<Self> {
        let assets = ASSETS.get()?;
        let set = &assets.syntaxes;
        let by = |s: Option<&str>| s.and_then(|s| set.find_syntax_by_extension(s));
        let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
        let syntax = by(path.and_then(Path::file_name).and_then(|n| n.to_str()))
            .or_else(|| by(path.and_then(Path::extension).and_then(|e| e.to_str())))
            .or_else(|| {
                let end = memchr::memchr(b'\n', head)?;
                set.find_syntax_by_first_line(std::str::from_utf8(&head[..end]).ok()?)
            })
            .or_else(|| match format::detect(None, head)? {
                format::Kind::Json => set.find_syntax_by_name("JSON"),
                format::Kind::Xml => set.find_syntax_by_name("XML"),
            })?;
        if syntax.name == set.find_syntax_plain_text().name {
            return None;
        }
        let start = State::new(syntax, &Highlighter::new(&assets.theme));
        Some(Self {
            syntax,
            snapshots: vec![start.clone()],
            frontier: (0, start),
            settled: false,
            cursor: None,
            cache: HashMap::new(),
        })
    }

    /// The grammar's name: `JSON`, `Rust`.
    pub fn name(&self) -> &str {
        &self.syntax.name
    }

    /// Whether the parse from the top has finished, or is not wanted.
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    /// Parses on from the top until `deadline`. Returns whether it has
    /// finished. It waits for the line index, which it reads through.
    pub fn advance(&mut self, doc: &mut Document, deadline: Instant) -> io::Result<bool> {
        if self.settled {
            return Ok(true);
        }
        if doc.len() > BACKGROUND_LIMIT {
            self.settled = true;
            return Ok(true);
        }
        if !doc.is_indexed() {
            return Ok(false);
        }
        let assets = assets();
        let highlighter = Highlighter::new(&assets.theme);
        let (mut line, mut state) = (self.frontier.0, self.frontier.1.clone());
        let snapshots = &mut self.snapshots;
        let mut scratch = String::new();
        let mut parsed = 0u32;
        let ended = each_line(doc, line, |i, text| {
            if i % STRIDE == 0 && snapshots.len() as u64 == i / STRIDE {
                snapshots.push(state.clone());
            }
            state.feed(text, &highlighter, assets, &mut scratch, None);
            line = i + 1;
            parsed += 1;
            // The clock is read every so many lines, not every one.
            !(parsed % 64 == 0 && Instant::now() >= deadline)
        })?;
        self.frontier = (line, state);
        self.settled = ended;
        Ok(ended)
    }

    /// Appends the runs of `line` to `out`: exact where the parse from the
    /// top has passed or can be reached from, guessed otherwise.
    pub fn runs(&mut self, doc: &mut Document, line: u64, out: &mut Vec<Run>) -> io::Result<()> {
        if let Some(hit) = self.cache.get(&line)
            && (hit.exact || line >= self.frontier.0)
        {
            out.extend_from_slice(&hit.runs);
            return Ok(());
        }
        if self.cache.len() > CACHE_LIMIT {
            self.cache.clear();
        }
        let assets = assets();
        let highlighter = Highlighter::new(&assets.theme);
        let (start, mut state, exact) = self.start_for(line, &highlighter);
        let cache = &mut self.cache;
        let mut scratch = String::new();
        let mut next = start;
        each_line(doc, start, |i, text| {
            let mut runs = Vec::new();
            state.feed(text, &highlighter, assets, &mut scratch, Some(&mut runs));
            cache.insert(i, Cached { runs, exact });
            next = i + 1;
            i < line + PREFETCH
        })?;
        self.cursor = Some((next, state, exact));
        if let Some(hit) = self.cache.get(&line) {
            out.extend_from_slice(&hit.runs);
        }
        Ok(())
    }

    /// Where to parse from for `line`, in what state, and whether that state
    /// is known to be right.
    fn start_for(&self, line: u64, highlighter: &Highlighter) -> (u64, State, bool) {
        let near = |from: u64| from <= line && line - from <= LOOKBACK;
        if let Some((next, state, exact)) = &self.cursor
            && near(*next)
            && (*exact || line >= self.frontier.0)
        {
            return (*next, state.clone(), *exact);
        }
        let (front, front_state) = &self.frontier;
        if line < *front {
            let k = ((line / STRIDE) as usize).min(self.snapshots.len() - 1);
            return (k as u64 * STRIDE, self.snapshots[k].clone(), true);
        }
        if near(*front) {
            return (*front, front_state.clone(), true);
        }
        let start = line.saturating_sub(LOOKBACK);
        (start, State::new(self.syntax, highlighter), start == 0)
    }

    /// Forgets what the text from `line` on was parsed to: it has changed.
    /// A snapshot at or above `line` holds only what the lines above it made
    /// of the state, so those are kept, and the parse from the top resumes
    /// from the last of them.
    pub fn invalidate_from(&mut self, line: u64) {
        self.snapshots.truncate((line / STRIDE) as usize + 1);
        let k = self.snapshots.len() - 1;
        self.frontier = (k as u64 * STRIDE, self.snapshots[k].clone());
        self.settled = false;
        self.cursor = None;
        self.cache.clear();
    }
}

/// Hands `f` each line from `from` on, in order — `None` for one longer than
/// [`MAX_LINE`] — until `f` answers `false` or the document ends. Returns
/// whether it reached the end.
///
/// Reads the document a chunk at a time rather than asking it for a line at
/// a time: finding a line through the index scans from its checkpoint, which
/// a run of lines would pay for over and over. Lines are decoded as the
/// document decodes them, so byte offsets agree with what is drawn.
fn each_line(
    doc: &mut Document,
    from: u64,
    mut f: impl FnMut(u64, Option<&str>) -> bool,
) -> io::Result<bool> {
    let Some(mut off) = doc.line_start(from)? else {
        return Ok(true);
    };
    let mut buf = vec![0u8; 64 * 1024];
    let mut pending: Vec<u8> = Vec::new();
    let mut long = false;
    let mut line = from;
    loop {
        let n = doc.read_at(off, &mut buf)?;
        if n == 0 {
            let text = (!long).then(|| String::from_utf8_lossy(&pending));
            f(line, text.as_deref());
            return Ok(true);
        }
        off += n as u64;
        let chunk = &buf[..n];
        let mut start = 0;
        for i in memchr::memchr_iter(b'\n', chunk) {
            take(&mut pending, &mut long, &chunk[start..i]);
            let text = (!long).then(|| String::from_utf8_lossy(&pending));
            if !f(line, text.as_deref()) {
                return Ok(false);
            }
            pending.clear();
            long = false;
            line += 1;
            start = i + 1;
        }
        take(&mut pending, &mut long, &chunk[start..]);
    }
}

/// Adds `bytes` to a line being read, unless it has grown too long to parse.
fn take(pending: &mut Vec<u8>, long: &mut bool, bytes: &[u8]) {
    if *long {
        return;
    }
    if pending.len() + bytes.len() > MAX_LINE {
        *long = true;
        pending.clear();
    } else {
        pending.extend_from_slice(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn doc(text: &str) -> Document {
        let mut d = Document::from_text(text);
        d.index_complete().unwrap();
        d
    }

    fn rust() -> Syntax {
        load();
        Syntax::detect(Some(Path::new("main.rs")), b"").expect("Rust")
    }

    fn runs(syntax: &mut Syntax, doc: &mut Document, line: u64) -> Vec<Run> {
        let mut out = Vec::new();
        syntax.runs(doc, line, &mut out).unwrap();
        out
    }

    fn colour_at(runs: &[Run], byte: usize) -> Option<[u8; 3]> {
        runs.iter()
            .find(|r| r.start <= byte && byte < r.end)
            .map(|r| r.rgb)
    }

    #[test]
    fn picks_a_grammar_by_name_extension_first_line_or_content() {
        load();
        let name = |path: Option<&str>, head: &str| {
            Syntax::detect(path.map(Path::new), head.as_bytes()).map(|s| s.name().to_string())
        };
        assert_eq!(name(Some("main.rs"), "").as_deref(), Some("Rust"));
        assert_eq!(
            name(Some("run"), "#!/usr/bin/env python3\nprint(1)\n").as_deref(),
            Some("Python")
        );
        assert_eq!(name(Some("dump"), "{\"a\":1}").as_deref(), Some("JSON"));
        assert_eq!(name(None, "<rss/>").as_deref(), Some("XML"));
        assert_eq!(
            name(Some("notes.txt"), "hello"),
            None,
            "plain text is left alone"
        );
        assert_eq!(name(None, "hello"), None);
    }

    #[test]
    fn logs_and_plain_text_never_load_the_grammars() {
        assert!(!worth_loading(Some(Path::new("server.log")), b"{\"a\":1}"));
        assert!(!worth_loading(Some(Path::new("README")), b"hello"));
        assert!(worth_loading(Some(Path::new("README")), b"#!/bin/sh\n"));
        assert!(worth_loading(Some(Path::new("dump")), b"[1,2]"));
        assert!(worth_loading(Some(Path::new("lib.rs")), b""));
    }

    #[test]
    fn colours_the_tokens_of_a_line_in_order() {
        let mut s = rust();
        let mut d = doc("fn main() { let x = 42; }\n");
        let r = runs(&mut s, &mut d, 0);
        let keyword = colour_at(&r, 0).expect("`fn` is coloured");
        let number = colour_at(&r, 20).expect("`42` is coloured");
        assert_ne!(keyword, number);
        assert!(r.iter().all(|run| run.start < run.end));
        assert!(
            r.windows(2).all(|w| w[0].end <= w[1].start),
            "in order, without overlaps"
        );
    }

    #[test]
    fn a_comment_opened_far_above_is_guessed_then_put_right() {
        // A block comment opened on line 0 and closed 700 lines down: line
        // 500 is inside it, far below where a guess would start parsing.
        let mut text = String::from("/*\n");
        for i in 1..700 {
            text.push_str(&format!("let x{i} = {i};\n"));
        }
        text.push_str("*/\nlet y = 1;\n");
        let mut d = doc(&text);
        let mut s = rust();

        let comment = colour_at(&runs(&mut s, &mut d, 0), 0).expect("the opening is a comment");
        assert_ne!(
            colour_at(&runs(&mut s, &mut d, 500), 0),
            Some(comment),
            "before the parse from the top gets there, line 500 is guessed to be code"
        );

        let deadline = Instant::now() + Duration::from_secs(60);
        while !s.advance(&mut d, deadline).unwrap() {}
        assert!(s.snapshots.len() > 1, "snapshots were kept on the way");
        assert_eq!(
            colour_at(&runs(&mut s, &mut d, 500), 0),
            Some(comment),
            "once it has, line 500 is inside the comment"
        );

        // Closing the comment on line 1 changes every line after it.
        d.insert(3, "*/\n").unwrap();
        s.invalidate_from(1);
        assert_ne!(colour_at(&runs(&mut s, &mut d, 501), 0), Some(comment));
        while !s.advance(&mut d, deadline).unwrap() {}
        assert_ne!(colour_at(&runs(&mut s, &mut d, 501), 0), Some(comment));
    }

    #[test]
    fn a_line_too_long_to_parse_is_left_plain_and_the_next_is_not() {
        let mut s = rust();
        let long = "z".repeat(MAX_LINE * 2);
        let mut d = doc(&format!("let a = 1;\nlet b = \"{long}\";\nlet c = 3;\n"));
        assert!(runs(&mut s, &mut d, 1).is_empty());
        assert!(colour_at(&runs(&mut s, &mut d, 2), 0).is_some());
    }

    #[test]
    fn a_document_too_big_for_the_background_settles_at_once() {
        let mut s = rust();
        let mut d = doc("fn a() {}\n");
        assert!(!s.is_settled());
        assert!(s.advance(&mut d, Instant::now()).unwrap());
        assert!(s.is_settled(), "a small one finishes");
    }

    #[test]
    fn lines_are_read_in_order_as_the_document_has_them() {
        let mut d = doc("one\ntwo\n\nfour");
        d.insert(4, "2\n").unwrap();
        let mut seen = Vec::new();
        let ended = each_line(&mut d, 1, |i, text| {
            seen.push((i, text.unwrap().to_string()));
            true
        })
        .unwrap();
        assert!(ended);
        let expected: Vec<(u64, String)> = [(1, "2"), (2, "two"), (3, ""), (4, "four")]
            .into_iter()
            .map(|(i, t)| (i, t.to_string()))
            .collect();
        assert_eq!(seen, expected);

        let mut first = Vec::new();
        let ended = each_line(&mut d, 0, |i, _| {
            first.push(i);
            i < 1
        })
        .unwrap();
        assert!(!ended, "stopping early is not the end");
        assert_eq!(first, [0, 1]);
    }
}
