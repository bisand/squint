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
use std::fs;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// A format in place, held so one undo takes it back and one redo puts it on
/// again.
///
/// ⇧⌘F streams the file to a formatted copy beside it and the tab goes on
/// showing the same file, now read from that copy; this holds whichever of
/// the two documents is not the one on show. Both read their own file
/// through their own handle, so swapping them costs nothing.
struct Formatted {
    /// The document not on show: the unformatted one while `on` is true.
    other: Document,
    /// Whether the formatted one is the one on show.
    on: bool,
    /// The file the formatted bytes were streamed to, until a save renames
    /// it onto the real file. Deleted with the document when it is still
    /// there.
    part: Option<PathBuf>,
}

impl Drop for Formatted {
    /// Takes the formatted copy with it. The document reading that copy is
    /// this document's own and is dropped just before this, so the file is
    /// closed by the time it goes; a file the platform will not remove while
    /// something has it open is left behind, which is a stray file in a
    /// temporary directory and not worth a word to anybody.
    fn drop(&mut self) {
        if let Some(part) = self.part.take() {
            let _ = fs::remove_file(part);
        }
    }
}

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
    /// Whether this document is to be coloured at all: the settings say so,
    /// and it is not too big for them.
    syntax_allowed: bool,
    /// Runs asked for, kept so a paint does not allocate a vector per line.
    runs: Vec<Run>,
    /// A ⇧⌘F that has not been saved yet.
    formatted: Option<Formatted>,
    /// The document was swapped whole and the view above it is looking at
    /// line numbers that no longer mean anything.
    view_stale: bool,
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
            syntax_allowed: true,
            runs: Vec::new(),
            formatted: None,
            view_stale: false,
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
            syntax_allowed: true,
            runs: Vec::new(),
            formatted: None,
            view_stale: false,
        }
    }

    /// Whether syntect may colour this document. Turning it off drops the
    /// colours; turning it on has the grammar decided on again, which is also
    /// how a document takes up a theme chosen since it was parsed.
    pub fn allow_syntax(&mut self, allowed: bool) {
        self.syntax_allowed = allowed;
        self.syntax = None;
        self.syntax_decided = !allowed || self.path.is_none();
    }

    /// How many bytes the file is.
    pub fn len(&self) -> u64 {
        self.doc.len()
    }

    /// Decides on a grammar, once the grammars are loaded — starting their
    /// load if this file is worth it. Returns whether the text is coloured
    /// now, so the caller can say so.
    pub fn decide_syntax(&mut self) -> bool {
        if self.syntax_decided || !self.syntax_allowed {
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

    /// Takes a format in place back, or puts it on again: the two documents
    /// change places. `back` asks for the unformatted one. Whether there was
    /// one to swap to, which is what tells undo and redo they had something
    /// to do.
    fn swap_format(&mut self, back: bool) -> bool {
        let Some(state) = &mut self.formatted else {
            return false;
        };
        if state.on != back {
            return false;
        }
        std::mem::swap(&mut self.doc, &mut state.other);
        state.on = !back;
        self.changed_from(0);
        self.view_stale = true;
        true
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
    ///
    /// A file formatted in place and not typed in since is already a file of
    /// its own beside this one, so it goes into place with a rename: no
    /// second pass over the document, whatever it weighs.
    pub fn save(&mut self) -> Result<(), String> {
        let Some(path) = self.path.clone() else {
            return Err("this document has no file name".into());
        };
        if self.rename_formatted_onto(&path) {
            return Ok(());
        }
        self.doc
            .save_to(&path)
            .map_err(|e| format!("saving {}: {e}", path.display()))?;
        self.formatted = None;
        Ok(())
    }

    /// Writes the document to `path` and makes that its file from now on:
    /// where Save goes and what decides the grammar.
    pub fn save_as(&mut self, path: &Path) -> Result<(), String> {
        self.doc
            .save_to(path)
            .map_err(|e| format!("saving {}: {e}", path.display()))?;
        self.formatted = None;
        if self.path.as_deref() != Some(path) {
            self.path = Some(path.to_path_buf());
            self.syntax = None;
            self.syntax_decided = false;
        }
        Ok(())
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

    /// Takes up `formatted`, this document's file laid out again, as what the
    /// document holds from now on — without changing what file it is: the tab
    /// goes on showing the same name, now with unsaved changes in it.
    ///
    /// `part` is the copy on disk `formatted` reads, where there is one; a
    /// projection reads the original file and has none, so it passes `None`
    /// and its save streams rather than renaming. Either way the document put
    /// aside is kept, so one undo takes the format back.
    pub fn format_in_place(&mut self, mut formatted: Document, part: Option<PathBuf>) {
        formatted.mark_modified();
        let replaced = std::mem::replace(&mut self.doc, formatted);
        let prior = self.formatted.take();
        let other = match prior {
            // Formatted again. What this replaces is an earlier format, whose
            // copy is finished with; the document held back stays the one
            // that was never formatted.
            Some(mut prior) if prior.on => {
                drop(replaced);
                prior.part.take().inspect(|old| drop(fs::remove_file(old)));
                // `Formatted` clears up after itself when it is dropped, so
                // what it holds is taken out rather than moved out.
                std::mem::replace(&mut prior.other, Document::from_text(""))
            }
            // Formatted after an undo took an earlier format back, so what
            // this replaces is the unformatted document after all.
            Some(mut prior) => {
                prior.part.take().inspect(|old| drop(fs::remove_file(old)));
                replaced
            }
            None => replaced,
        };
        // Redo comes after the format, never before it.
        let mut other = other;
        other.clear_redo();
        self.formatted = Some(Formatted {
            other,
            on: true,
            part,
        });
        self.changed_from(0);
        self.view_stale = true;
    }

    /// Drops the copy an unsaved format is being read from, for a squint on
    /// its way out: on macOS the application menu's Quit ends the process
    /// where it stands, so nothing here is dropped by itself and the copy
    /// would be left beside the user's file.
    pub fn discard_format(&mut self) {
        self.formatted = None;
    }

    /// Whether the document was swapped whole since this was last asked, so
    /// the view above it is showing line numbers from the other one.
    pub fn take_view_stale(&mut self) -> bool {
        std::mem::take(&mut self.view_stale)
    }

    /// Renames a finished format onto `path`, when there is one and nothing
    /// has been typed since. Whether it did — a rename that cannot be done,
    /// because the copy had to go somewhere on another filesystem, leaves the
    /// ordinary save to do the work.
    fn rename_formatted_onto(&mut self, path: &Path) -> bool {
        let Some(state) = &self.formatted else {
            return false;
        };
        if !state.on || self.doc.is_edited() {
            return false;
        }
        let Some(part) = state.part.clone() else {
            return false;
        };
        if fs::rename(&part, path).is_err() {
            return false;
        }
        // The copy is the file now, so nothing is left to clear up; and the
        // document goes on reading the same bytes through the handle it
        // opened, because the rename moved the name, not the file.
        if let Some(state) = &mut self.formatted {
            state.part = None;
        }
        self.doc.mark_saved();
        self.formatted = None;
        true
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
    // With nothing typed left to take back, the next thing to undo is a
    // format in place, which swaps the whole document rather than its pieces.
    fn undo(&mut self) -> bool {
        if self.doc.undo() {
            self.changed_from(0);
            return true;
        }
        self.swap_format(true)
    }

    fn redo(&mut self) -> bool {
        if self.doc.redo() {
            self.changed_from(0);
            return true;
        }
        self.swap_format(false)
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

#[cfg(test)]
mod tests {
    use super::*;
    use squint_core::format::format_document;

    /// A file of `text`, and a formatted copy of it beside it, as ⇧⌘F leaves
    /// them: the copy's path and the document showing the file.
    fn formatted(dir: &Path, text: &str) -> (PathBuf, PathBuf, FileDocument) {
        let file = dir.join("dump.json");
        fs::write(&file, text).unwrap();
        let part = dir.join(".dump.json.squint-formatted");
        let doc = Document::open(&file).unwrap();
        let mut out = fs::File::create(&part).unwrap();
        format_document(&doc, Kind::Json, Style::default(), &mut out).unwrap();
        drop(out);
        let opened = FileDocument::open(&file).unwrap();
        (file, part, opened)
    }

    /// Counts the whole document, as the app does in slices between frames:
    /// a line past the first is not there to be read until the index is.
    fn index(doc: &mut FileDocument) {
        while !doc.index_step(1 << 20) {}
    }

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("squint-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_format_in_place_keeps_the_file_and_shows_unsaved_changes() {
        let dir = dir("in-place");
        let (file, part, mut doc) = formatted(&dir, r#"{"a":[1,2]}"#);
        assert!(!doc.is_modified());
        assert_eq!(doc.line(0).as_deref(), Some(r#"{"a":[1,2]}"#));

        doc.format_in_place(Document::open(&part).unwrap(), Some(part.clone()));
        assert_eq!(doc.path(), Some(file.as_path()), "the same file");
        assert!(doc.is_modified(), "the file on disk is not this");
        assert!(
            doc.take_view_stale(),
            "the line numbers mean something else"
        );
        assert_eq!(doc.line(0).as_deref(), Some("{"));
        index(&mut doc);
        assert_eq!(doc.line(1).as_deref(), Some(r#"  "a": ["#));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            r#"{"a":[1,2]}"#,
            "nothing has been written to the file yet"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_a_format_renames_the_copy_onto_the_file() {
        let dir = dir("rename");
        let (file, part, mut doc) = formatted(&dir, r#"{"a":[1,2]}"#);
        doc.format_in_place(Document::open(&part).unwrap(), Some(part.clone()));
        doc.save().unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "{\n  \"a\": [\n    1,\n    2\n  ]\n}\n"
        );
        assert!(!part.exists(), "the copy is the file now");
        assert!(!doc.is_modified());
        index(&mut doc);
        assert_eq!(doc.line(1).as_deref(), Some(r#"  "a": ["#));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn typing_after_a_format_writes_the_document_and_leaves_no_copy() {
        let dir = dir("edited");
        let (file, part, mut doc) = formatted(&dir, r#"{"a":1}"#);
        doc.format_in_place(Document::open(&part).unwrap(), Some(part.clone()));
        doc.insert(Pos::new(0, 1), "\n  \"b\": 2,");
        doc.save().unwrap();
        let written = fs::read_to_string(&file).unwrap();
        assert!(written.starts_with("{\n  \"b\": 2,"), "{written}");
        assert!(written.contains(r#""a": 1"#), "{written}");
        assert!(!part.exists(), "the copy is finished with");
        assert!(!doc.is_modified());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_undo_takes_a_format_back_and_one_redo_puts_it_on() {
        let dir = dir("undo");
        let (_, part, mut doc) = formatted(&dir, r#"{"a":[1,2]}"#);
        doc.format_in_place(Document::open(&part).unwrap(), Some(part.clone()));
        doc.insert(Pos::new(0, 1), "X");
        assert_eq!(doc.line(0).as_deref(), Some("{X"));

        assert!(doc.undo(), "the typing");
        assert_eq!(doc.line(0).as_deref(), Some("{"));
        assert!(doc.is_modified(), "the format is still on");

        assert!(doc.undo(), "the format");
        assert_eq!(doc.line(0).as_deref(), Some(r#"{"a":[1,2]}"#));
        assert!(!doc.is_modified(), "back to the file as it is");
        assert!(doc.take_view_stale());

        assert!(!doc.undo(), "and there was nothing before that");

        assert!(doc.redo(), "the format again");
        assert_eq!(doc.line(0).as_deref(), Some("{"));
        assert!(doc.is_modified());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_document_dropped_takes_its_unsaved_copy_with_it() {
        let dir = dir("drop");
        let (_, part, mut doc) = formatted(&dir, r#"{"a":1}"#);
        doc.format_in_place(Document::open(&part).unwrap(), Some(part.clone()));
        assert!(part.exists());
        drop(doc);
        assert!(!part.exists(), "an unsaved format leaves nothing behind");
        let _ = fs::remove_dir_all(&dir);
    }
}
