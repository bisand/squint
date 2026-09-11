//! The engine's document, as the text area sees it.
//!
//! `squint_core::Document` speaks byte offsets and `io::Result`; the widget
//! speaks `(line, column)` and has no error channel. This adapter maps one to
//! the other, keeps the last I/O error for the status bar, and holds the path
//! the document came from so it can go back there.

use denise::Color;
use denise_ui::widgets::{Pos, Span, TextDocument};
use squint_core::format::{Format, Kind, Style};
use squint_core::syntax::{self, Run, Syntax};
use squint_core::{Document, Find, FindStep, Needle};
use std::borrow::Cow;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub struct FileDocument {
    doc: Document,
    path: Option<PathBuf>,
    /// The last thing that went wrong, until it is read.
    error: Option<String>,
    /// What to mark on the lines on screen: the query being found.
    highlight: Option<Needle>,
    /// The grammar colouring the text, once one is decided on.
    syntax: Option<Syntax>,
    /// Whether the grammar has been decided on, which waits for the grammars
    /// to load.
    syntax_decided: bool,
    /// Runs asked for, kept so a paint does not allocate a vector per line.
    runs: Vec<Run>,
}

impl FileDocument {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            doc: Document::open(path)?,
            path: Some(path.to_path_buf()),
            error: None,
            highlight: None,
            syntax: None,
            syntax_decided: false,
            runs: Vec::new(),
        })
    }

    pub fn empty() -> Self {
        Self {
            doc: Document::from_text(""),
            path: None,
            error: None,
            highlight: None,
            syntax: None,
            syntax_decided: true,
            runs: Vec::new(),
        }
    }

    /// Decides on a grammar, once the grammars are loaded — starting their
    /// load if this file is worth it. Returns whether the text is coloured
    /// now, so the caller can say so.
    pub fn decide_syntax(&mut self) -> bool {
        if self.syntax_decided {
            return false;
        }
        let head = self.head(8192);
        if !syntax::worth_loading(self.path.as_deref(), &head) {
            self.syntax_decided = true;
            return false;
        }
        if !syntax::is_loaded() {
            syntax::load_in_background();
            return false;
        }
        self.syntax_decided = true;
        self.syntax = Syntax::detect(self.path.as_deref(), &head);
        self.syntax.is_some()
    }

    /// Whether a grammar is still to be decided on.
    pub fn syntax_undecided(&self) -> bool {
        !self.syntax_decided
    }

    /// The grammar colouring the text: `JSON`, `Rust`.
    pub fn syntax_name(&self) -> Option<&str> {
        self.syntax.as_ref().map(Syntax::name)
    }

    /// Whether the parse from the top has work left.
    pub fn highlighting(&self) -> bool {
        self.syntax.as_ref().is_some_and(|s| !s.is_settled())
    }

    /// Parses on from the top until `deadline`. An I/O error turns the
    /// colouring off and is kept for the status line.
    pub fn highlight_until(&mut self, deadline: Instant) {
        if let Some(syntax) = &mut self.syntax
            && let Err(e) = syntax.advance(&mut self.doc, deadline)
        {
            self.error = Some(format!("highlighting: {e}"));
            self.syntax = None;
        }
    }

    /// The text changed from `line` on.
    fn changed_from(&mut self, line: usize) {
        if let Some(syntax) = &mut self.syntax {
            syntax.invalidate_from(line as u64);
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_modified(&self) -> bool {
        self.doc.is_modified()
    }

    pub fn is_indexed(&self) -> bool {
        self.doc.is_indexed()
    }

    /// Bytes scanned so far, out of the original's length.
    pub fn index_progress(&self) -> (u64, u64) {
        self.doc.index_progress()
    }

    /// Scans up to `budget` more bytes of the original. Returns whether the
    /// index is complete.
    pub fn index_step(&mut self, budget: usize) -> bool {
        match self.doc.index_step(budget) {
            Ok(done) => done,
            Err(e) => {
                self.error = Some(format!("indexing: {e}"));
                true
            }
        }
    }

    pub fn memory_bytes(&self) -> usize {
        self.doc.memory_bytes()
    }

    /// Writes the document back to its file.
    pub fn save(&mut self) -> Result<(), String> {
        let Some(path) = self.path.clone() else {
            return Err("this document has no file name".into());
        };
        self.doc
            .save_to(&path)
            .map_err(|e| format!("saving {}: {e}", path.display()))
    }

    /// The last error, cleared on the way out.
    pub fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    /// The document offset of `at`, or `None` if its line is not known yet.
    pub fn offset_of(&mut self, at: Pos) -> Option<u64> {
        match self.doc.line_start(at.line as u64) {
            Ok(start) => start.map(|s| s + at.col as u64),
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        }
    }

    /// The position of document offset `off`, or `None` if the index has not
    /// reached it.
    pub fn pos_of(&mut self, off: u64) -> Option<Pos> {
        match self.doc.line_col_of(off) {
            Ok(at) => at.map(|(line, col)| Pos::new(line as usize, col as usize)),
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        }
    }

    /// A search for `query` from offset `from`.
    pub fn find(&self, query: &str, from: u64, forward: bool) -> Find {
        Find::new(&self.doc, query, from, forward)
    }

    /// Advances `find` by up to `budget` bytes. An I/O error ends the search
    /// and is kept for the status line.
    pub fn find_step(&mut self, find: &mut Find, budget: usize) -> FindStep {
        match find.step(&self.doc, budget) {
            Ok(step) => step,
            Err(e) => {
                self.error = Some(format!("finding: {e}"));
                FindStep::NotFound
            }
        }
    }

    /// Marks every match of `query` on the lines drawn, by the same rules a
    /// find uses; an empty query marks nothing.
    pub fn set_highlight(&mut self, query: &str) {
        self.highlight = (!query.is_empty()).then(|| Needle::new(query));
    }

    /// The first `n` bytes, for telling what kind of file this is.
    pub fn head(&self, n: usize) -> Vec<u8> {
        self.doc.read(0, n).unwrap_or_default()
    }

    /// A format of this document as `kind`, to be stepped with
    /// [`format_step`](Self::format_step).
    pub fn format(&self, kind: Kind, style: Style) -> Format {
        Format::new(&self.doc, kind, style)
    }

    /// Advances `job` by up to `budget` bytes, writing into `w`. Returns
    /// whether it has finished.
    pub fn format_step<W: Write>(
        &self,
        job: &mut Format,
        budget: usize,
        w: &mut W,
    ) -> std::io::Result<bool> {
        job.step(&self.doc, budget, w)
    }
}

impl TextDocument for FileDocument {
    fn line_count(&mut self) -> Option<usize> {
        self.doc.line_count().ok().flatten().map(|n| n as usize)
    }

    fn known_lines(&mut self) -> usize {
        self.doc.known_lines().map_or(1, |n| n as usize)
    }

    fn line(&mut self, n: usize) -> Option<Cow<'_, str>> {
        match self.doc.line(n as u64) {
            Ok(line) => line.map(Cow::Owned),
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        }
    }

    fn insert(&mut self, at: Pos, text: &str) {
        if let Some(off) = self.offset_of(at)
            && let Err(e) = self.doc.insert(off, text)
        {
            self.error = Some(e.to_string());
        }
        self.changed_from(at.line);
    }

    fn delete(&mut self, from: Pos, to: Pos) {
        let (Some(a), Some(b)) = (self.offset_of(from), self.offset_of(to)) else {
            return;
        };
        if b > a
            && let Err(e) = self.doc.delete(a, b - a)
        {
            self.error = Some(e.to_string());
        }
        self.changed_from(from.line);
    }

    // Undo and redo do not say where the text changed, so all of it may have.
    fn undo(&mut self) -> bool {
        let undone = self.doc.undo();
        if undone {
            self.changed_from(0);
        }
        undone
    }

    fn redo(&mut self) -> bool {
        let redone = self.doc.redo();
        if redone {
            self.changed_from(0);
        }
        redone
    }

    fn spans(&mut self, n: usize, out: &mut Vec<Span>) {
        let Some(syntax) = &mut self.syntax else {
            return;
        };
        self.runs.clear();
        if let Err(e) = syntax.runs(&mut self.doc, n as u64, &mut self.runs) {
            self.error = Some(format!("highlighting: {e}"));
            return;
        }
        out.extend(self.runs.iter().map(|run| Span {
            start: run.start,
            end: run.end,
            color: Color::rgb(run.rgb[0], run.rgb[1], run.rgb[2]),
        }));
    }

    fn highlights(&mut self, _n: usize, line: &str, out: &mut Vec<Range<usize>>) {
        if let Some(needle) = &self.highlight {
            needle.matches_in(line.as_bytes(), out);
        }
    }
}
