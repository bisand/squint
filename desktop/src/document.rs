//! The engine's document, as the text area sees it.
//!
//! `squint_core::Document` speaks byte offsets and `io::Result`; the widget
//! speaks `(line, column)` and has no error channel. This adapter maps one to
//! the other, keeps the last I/O error for the status bar, and holds the path
//! the document came from so it can go back there.

use denise_ui::widgets::{Pos, TextDocument};
use squint_core::{Document, Find, FindStep, Needle};
use std::borrow::Cow;
use std::ops::Range;
use std::path::{Path, PathBuf};

pub struct FileDocument {
    doc: Document,
    path: Option<PathBuf>,
    /// The last thing that went wrong, until it is read.
    error: Option<String>,
    /// What to mark on the lines on screen: the query being found.
    highlight: Option<Needle>,
}

impl FileDocument {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            doc: Document::open(path)?,
            path: Some(path.to_path_buf()),
            error: None,
            highlight: None,
        })
    }

    pub fn empty() -> Self {
        Self {
            doc: Document::from_text(""),
            path: None,
            error: None,
            highlight: None,
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
    }

    fn undo(&mut self) -> bool {
        self.doc.undo()
    }

    fn redo(&mut self) -> bool {
        self.doc.redo()
    }

    fn highlights(&mut self, _n: usize, line: &str, out: &mut Vec<Range<usize>>) {
        if let Some(needle) = &self.highlight {
            needle.matches_in(line.as_bytes(), out);
        }
    }
}
