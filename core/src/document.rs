//! The document: a piece table whose original buffer is the file on disk.
//!
//! The file is never loaded. Typed text goes into an append-only add buffer,
//! and the document is a sequence of pieces, each a range of either the file
//! or the add buffer. Reading a line reads only that line; saving streams the
//! pieces out; undo restores an earlier piece list, which stays valid because
//! the add buffer only grows.
//!
//! Each piece remembers how many newlines it holds so a line can be found by
//! walking pieces. A piece of the original file only knows that once the
//! [`LineIndex`] has scanned past it, so the count is optional: until the
//! scan is done the document can answer for the lines it has passed and says
//! `None` for the total. Edits split pieces, and splitting a piece of the
//! file counts newlines through the index rather than by reading the file
//! through.
//!
//! Pieces are kept in a `Vec`; every operation walks it. That is O(edits),
//! fine for the light editing squint is for, and the place to put a tree if
//! it ever is not.

use crate::index::{LineIndex, DEFAULT_STRIDE};
use crate::source::{after_nth_newline, FileSource, MemSource, Source};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Buf {
    Original,
    Added,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Piece {
    buf: Buf,
    start: u64,
    len: u64,
    /// `None` for a piece of the original the index has not reached.
    newlines: Option<u64>,
}

impl Piece {
    fn end(&self) -> u64 {
        self.start + self.len
    }
}

/// When a piece is split and its two halves' newlines are needed, ranges up
/// to this size are read and counted directly; larger ones go through the
/// index.
const DIRECT_COUNT_LIMIT: u64 = 1024 * 1024;

pub struct Document {
    source: Box<dyn Source>,
    index: LineIndex,
    added: Vec<u8>,
    pieces: Vec<Piece>,
    undo: Vec<Vec<Piece>>,
    redo: Vec<Vec<Piece>>,
    /// The pieces as they were at open or last save.
    clean: Vec<Piece>,
    /// Newlines before the first unresolved piece's start, keyed by that
    /// start, so the scroll extent can be asked every frame while indexing.
    base_cache: Option<(u64, u64)>,
}

impl Document {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self::from_source(Box::new(FileSource::open(path)?)))
    }

    pub fn from_text(text: &str) -> Self {
        Self::from_source(Box::new(MemSource(text.as_bytes().to_vec())))
    }

    pub fn from_source(source: Box<dyn Source>) -> Self {
        let len = source.len();
        let pieces = if len == 0 {
            vec![]
        } else {
            vec![Piece {
                buf: Buf::Original,
                start: 0,
                len,
                newlines: None,
            }]
        };
        Self {
            index: LineIndex::new(len, DEFAULT_STRIDE),
            source,
            added: Vec::new(),
            clean: pieces.clone(),
            pieces,
            undo: Vec::new(),
            redo: Vec::new(),
            base_cache: None,
        }
    }

    // ---- indexing -------------------------------------------------------

    /// Scans up to `budget` more bytes of the original. Returns whether the
    /// index is complete. Call from a worker thread, or in slices between
    /// frames, until it returns `true`.
    pub fn index_step(&mut self, budget: usize) -> io::Result<bool> {
        let done = self.index.advance(&*self.source, budget)?;
        self.base_cache = None;
        Ok(done)
    }

    pub fn index_complete(&mut self) -> io::Result<()> {
        while !self.index_step(4 * 1024 * 1024)? {}
        Ok(())
    }

    pub fn is_indexed(&self) -> bool {
        self.index.is_complete()
    }

    /// Bytes scanned by the index so far, out of the original's length.
    pub fn index_progress(&self) -> (u64, u64) {
        (self.index.scanned(), self.source.len())
    }

    /// Fills in the newline count of every piece the index has now passed.
    fn resolve(&mut self) -> io::Result<()> {
        let scanned = self.index.scanned();
        for i in 0..self.pieces.len() {
            let p = &self.pieces[i];
            if p.newlines.is_none() && p.buf == Buf::Original && p.end() <= scanned {
                let n = self.index.newlines_in(&*self.source, p.start, p.end())?;
                self.pieces[i].newlines = n;
            }
        }
        Ok(())
    }

    // ---- size and lines -------------------------------------------------

    pub fn len(&self) -> u64 {
        self.pieces.iter().map(|p| p.len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// Bytes the document holds in memory: the index, the add buffer and the
    /// piece lists, including undo history. The original is not counted; it
    /// is on disk.
    pub fn memory_bytes(&self) -> usize {
        let piece = std::mem::size_of::<Piece>();
        let lists = self.undo.iter().chain(&self.redo).map(|l| l.capacity()).sum::<usize>()
            + self.pieces.capacity()
            + self.clean.capacity();
        self.index.memory_bytes() + self.added.capacity() + lists * piece
    }

    /// Total lines, or `None` while the index has not reached the end. A
    /// document always has at least one line; a trailing newline ends a line
    /// and starts an empty last one.
    pub fn line_count(&mut self) -> io::Result<Option<u64>> {
        self.resolve()?;
        let mut total = 0;
        for p in &self.pieces {
            match p.newlines {
                Some(n) => total += n,
                None => return Ok(None),
            }
        }
        Ok(Some(total + 1))
    }

    /// Lines whose start is known now. Equals `line_count()` once indexed;
    /// grows while the index runs.
    pub fn known_lines(&mut self) -> io::Result<u64> {
        self.resolve()?;
        let mut total = 0;
        for p in &self.pieces {
            match p.newlines {
                Some(n) => total += n,
                None => {
                    let base = match self.base_cache {
                        Some((start, base)) if start == p.start => Some(base),
                        _ => self.index.newlines_before(&*self.source, p.start)?,
                    };
                    if let Some(base) = base {
                        self.base_cache = Some((p.start, base));
                        total += self.index.newlines() - base;
                    }
                    break;
                }
            }
        }
        Ok(total + 1)
    }

    /// Byte offset where 0-based line `n` starts, or `None` if the line is
    /// past the end or the index has not reached it.
    pub fn line_start(&mut self, n: u64) -> io::Result<Option<u64>> {
        self.resolve()?;
        let mut line = 0;
        let mut off = 0;
        for p in &self.pieces {
            match p.newlines {
                Some(nl) if n > line + nl => {
                    line += nl;
                    off += p.len;
                }
                _ => {
                    let rel = n - line;
                    return Ok(self.nth_line_in(p, rel)?.map(|r| off + r));
                }
            }
        }
        Ok((n == line).then_some(off))
    }

    /// Offset within `p` of its `rel`-th line: 0 for the line running into
    /// the piece, else just past its `rel`-th newline.
    fn nth_line_in(&self, p: &Piece, rel: u64) -> io::Result<Option<u64>> {
        if rel == 0 {
            return Ok(Some(0));
        }
        if let Some(nl) = p.newlines {
            if rel > nl {
                return Ok(None);
            }
        }
        match p.buf {
            Buf::Added => {
                let bytes = &self.added[p.start as usize..p.end() as usize];
                Ok(memchr::memchr_iter(b'\n', bytes)
                    .nth(rel as usize - 1)
                    .map(|i| i as u64 + 1))
            }
            Buf::Original => {
                // Through the index when it can answer, else a direct scan
                // bounded by the piece.
                if let Some(base) = self.index.newlines_before(&*self.source, p.start)? {
                    if let Some(abs) = self.index.line_start(&*self.source, base + rel)? {
                        // `abs == p.end()` is the line starting right after
                        // a newline that closes the piece.
                        return Ok((abs <= p.end()).then(|| abs - p.start));
                    }
                    if p.newlines.is_none() {
                        // The scan has not reached that line yet.
                        return Ok(None);
                    }
                }
                Ok(after_nth_newline(&*self.source, p.start, p.end(), rel)?.map(|a| a - p.start))
            }
        }
    }

    /// Line `n` without its newline, decoded lossily, or `None` if unknown.
    pub fn line(&mut self, n: u64) -> io::Result<Option<String>> {
        let Some(start) = self.line_start(n)? else {
            return Ok(None);
        };
        let bytes = self.read_line_at(start)?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Bytes from `off` up to, not including, the next newline or the end.
    pub fn read_line_at(&self, mut off: u64) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = self.read_at(off, &mut buf)?;
            if n == 0 {
                return Ok(out);
            }
            if let Some(i) = memchr::memchr(b'\n', &buf[..n]) {
                out.extend_from_slice(&buf[..i]);
                return Ok(out);
            }
            out.extend_from_slice(&buf[..n]);
            off += n as u64;
        }
    }

    /// Reads document bytes at `off`, across pieces, clipped to the end.
    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let mut filled = 0;
        let mut pos = 0;
        for p in &self.pieces {
            if filled == buf.len() {
                break;
            }
            let want_from = off + filled as u64;
            if want_from < pos + p.len && want_from >= pos {
                let rel = want_from - pos;
                let n = ((p.len - rel) as usize).min(buf.len() - filled);
                let got = self.read_piece(p, rel, &mut buf[filled..filled + n])?;
                if got == 0 {
                    break;
                }
                filled += got;
            }
            pos += p.len;
        }
        Ok(filled)
    }

    fn read_piece(&self, p: &Piece, rel: u64, buf: &mut [u8]) -> io::Result<usize> {
        match p.buf {
            Buf::Added => {
                let s = (p.start + rel) as usize;
                buf.copy_from_slice(&self.added[s..s + buf.len()]);
                Ok(buf.len())
            }
            Buf::Original => self.source.read_at(p.start + rel, buf),
        }
    }

    pub fn read(&self, off: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut out = vec![0; len];
        let n = self.read_at(off, &mut out)?;
        out.truncate(n);
        Ok(out)
    }

    // ---- editing --------------------------------------------------------

    fn snapshot(&mut self) {
        self.undo.push(self.pieces.clone());
        self.redo.clear();
    }

    /// Newlines in the first `rel` bytes of `p`.
    fn newlines_in_prefix(&self, p: &Piece, rel: u64) -> io::Result<Option<u64>> {
        match p.buf {
            Buf::Added => {
                let bytes = &self.added[p.start as usize..(p.start + rel) as usize];
                Ok(Some(bytecount::count(bytes, b'\n') as u64))
            }
            Buf::Original if rel <= DIRECT_COUNT_LIMIT => {
                Ok(Some(crate::source::count_newlines(&*self.source, p.start, p.start + rel)?))
            }
            Buf::Original => self.index.newlines_in(&*self.source, p.start, p.start + rel),
        }
    }

    /// Splits so that a piece boundary falls at `off`; returns the index of
    /// the piece starting there (`pieces.len()` for the end).
    fn split_at(&mut self, off: u64) -> io::Result<usize> {
        let mut pos = 0;
        for i in 0..self.pieces.len() {
            let p = self.pieces[i].clone();
            if off == pos {
                return Ok(i);
            }
            if off < pos + p.len {
                let rel = off - pos;
                let left_nl = self.newlines_in_prefix(&p, rel)?;
                let right_nl = match (p.newlines, left_nl) {
                    (Some(t), Some(l)) => Some(t - l),
                    _ => None,
                };
                let right = Piece {
                    buf: p.buf,
                    start: p.start + rel,
                    len: p.len - rel,
                    newlines: right_nl,
                };
                self.pieces[i].len = rel;
                self.pieces[i].newlines = left_nl;
                self.pieces.insert(i + 1, right);
                return Ok(i + 1);
            }
            pos += p.len;
        }
        assert_eq!(off, pos, "offset {off} past the end {pos}");
        Ok(self.pieces.len())
    }

    /// Inserts `text` at byte offset `off`, which must lie on a character
    /// boundary the caller has checked.
    pub fn insert(&mut self, off: u64, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.snapshot();
        let i = self.split_at(off)?;
        let start = self.added.len() as u64;
        self.added.extend_from_slice(text.as_bytes());
        let newlines = bytecount::count(text.as_bytes(), b'\n') as u64;
        // Typing extends the previous piece instead of making one per key.
        if i > 0 {
            let prev = &mut self.pieces[i - 1];
            if prev.buf == Buf::Added && prev.end() == start {
                prev.len += text.len() as u64;
                prev.newlines = prev.newlines.map(|n| n + newlines);
                return Ok(());
            }
        }
        self.pieces.insert(
            i,
            Piece {
                buf: Buf::Added,
                start,
                len: text.len() as u64,
                newlines: Some(newlines),
            },
        );
        Ok(())
    }

    /// Deletes `len` bytes at `off`.
    pub fn delete(&mut self, off: u64, len: u64) -> io::Result<()> {
        if len == 0 {
            return Ok(());
        }
        self.snapshot();
        let i = self.split_at(off)?;
        let j = self.split_at(off + len)?;
        self.pieces.drain(i..j);
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(p) => {
                self.redo.push(std::mem::replace(&mut self.pieces, p));
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(p) => {
                self.undo.push(std::mem::replace(&mut self.pieces, p));
                true
            }
            None => false,
        }
    }

    pub fn is_modified(&self) -> bool {
        self.pieces != self.clean
    }

    // ---- saving ---------------------------------------------------------

    /// Streams the document out, reading the original in chunks.
    pub fn write_to(&self, w: &mut dyn Write) -> io::Result<()> {
        const CHUNK: usize = 1024 * 1024;
        let mut buf = Vec::new();
        for p in &self.pieces {
            match p.buf {
                Buf::Added => w.write_all(&self.added[p.start as usize..p.end() as usize])?,
                Buf::Original => {
                    buf.resize(CHUNK.min(p.len as usize), 0);
                    let mut off = p.start;
                    while off < p.end() {
                        let want = ((p.end() - off) as usize).min(buf.len());
                        let n = self.source.read_at(off, &mut buf[..want])?;
                        if n == 0 {
                            return Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "the original file is shorter than when it was opened",
                            ));
                        }
                        w.write_all(&buf[..n])?;
                        off += n as u64;
                    }
                }
            }
        }
        Ok(())
    }

    /// Writes the whole document to a temporary file beside `path` and
    /// renames it into place, so a crash leaves either the old file or the
    /// new one, never a mix. The document keeps reading its original from the
    /// handle it opened (the old bytes stay reachable through it even when
    /// `path` is that file); reopen the saved file to let them go.
    pub fn save_to(&mut self, path: &Path) -> io::Result<()> {
        let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
        let mut tmp_name = name.to_os_string();
        tmp_name.push(".squint-tmp");
        let tmp = match dir {
            Some(d) => d.join(&tmp_name),
            None => Path::new(&tmp_name).to_path_buf(),
        };
        let result = (|| {
            let file = File::create(&tmp)?;
            let mut w = BufWriter::with_capacity(1024 * 1024, file);
            self.write_to(&mut w)?;
            let file = w.into_inner().map_err(|e| e.into_error())?;
            file.sync_all()?;
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        self.clean = self.pieces.clone();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(d: &Document) -> String {
        let mut out = Vec::new();
        d.write_to(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn lines_of(d: &mut Document) -> Vec<String> {
        let n = d.line_count().unwrap().unwrap();
        (0..n).map(|i| d.line(i).unwrap().unwrap()).collect()
    }

    fn indexed(text: &str) -> Document {
        let mut d = Document::from_text(text);
        d.index_complete().unwrap();
        d
    }

    #[test]
    fn reads_lines_of_an_untouched_document() {
        let mut d = indexed("one\ntwo\nthree");
        assert_eq!(d.len(), 13);
        assert_eq!(d.line_count().unwrap(), Some(3));
        assert_eq!(lines_of(&mut d), ["one", "two", "three"]);
        assert_eq!(d.line(3).unwrap(), None);

        let mut d = indexed("one\n");
        assert_eq!(lines_of(&mut d), ["one", ""]);
        let mut d = indexed("");
        assert_eq!(lines_of(&mut d), [""]);
        assert!(!d.is_modified());
    }

    #[test]
    fn edits_split_and_join_pieces() {
        let mut d = indexed("one\ntwo\nthree");
        d.insert(4, "2 and ").unwrap();
        assert_eq!(text_of(&d), "one\n2 and two\nthree");
        assert_eq!(lines_of(&mut d), ["one", "2 and two", "three"]);
        d.insert(4, "line ").unwrap();
        assert_eq!(text_of(&d), "one\nline 2 and two\nthree");
        assert!(d.is_modified());

        d.insert(3, "\n1b").unwrap();
        assert_eq!(lines_of(&mut d), ["one", "1b", "line 2 and two", "three"]);
        assert_eq!(d.line_count().unwrap(), Some(4));

        // Delete across the original / added seam and a newline.
        d.delete(2, 9).unwrap();
        assert_eq!(text_of(&d), "on 2 and two\nthree");
        assert_eq!(lines_of(&mut d), ["on 2 and two", "three"]);
    }

    #[test]
    fn typing_extends_the_last_piece() {
        let mut d = indexed("ab");
        for c in ["x", "y", "\n", "z"] {
            d.insert(d.len(), c).unwrap();
        }
        assert_eq!(d.pieces.len(), 2);
        assert_eq!(lines_of(&mut d), ["abxy", "z"]);
    }

    #[test]
    fn undo_and_redo_restore_piece_lists() {
        let mut d = indexed("hello");
        d.insert(5, " world").unwrap();
        d.delete(0, 1).unwrap();
        assert_eq!(text_of(&d), "ello world");
        assert!(d.undo());
        assert_eq!(text_of(&d), "hello world");
        assert!(d.undo());
        assert_eq!(text_of(&d), "hello");
        assert!(!d.is_modified());
        assert!(!d.undo());
        assert!(d.redo());
        assert_eq!(text_of(&d), "hello world");
        d.insert(0, "!").unwrap();
        assert!(!d.redo(), "a new edit drops the redo history");
    }

    #[test]
    fn line_count_is_unknown_until_indexed() {
        let text: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        let mut d = Document::from_text(&text);
        assert_eq!(d.line_count().unwrap(), None);
        assert_eq!(d.known_lines().unwrap(), 1);

        assert!(!d.index_step(40).unwrap()); // "line 1\n".."line 5\n" is 35 bytes
        assert_eq!(d.line_count().unwrap(), None);
        assert_eq!(d.known_lines().unwrap(), 6);
        assert_eq!(d.line(4).unwrap().as_deref(), Some("line 5"));
        assert_eq!(d.line(5).unwrap().as_deref(), Some("line 6"), "the line under scan is readable");
        assert_eq!(d.line(6).unwrap(), None);

        // Editing inside the scanned prefix works while the scan runs.
        d.insert(0, "top\n").unwrap();
        assert_eq!(d.known_lines().unwrap(), 7);
        assert_eq!(d.line(0).unwrap().as_deref(), Some("top"));
        assert_eq!(d.line(5).unwrap().as_deref(), Some("line 5"));
        assert_eq!(d.line_count().unwrap(), None);

        d.index_complete().unwrap();
        assert_eq!(d.line_count().unwrap(), Some(52));
        assert_eq!(d.known_lines().unwrap(), 52);
        assert_eq!(d.line(50).unwrap().as_deref(), Some("line 50"));
        assert_eq!(d.line(51).unwrap().as_deref(), Some(""));
    }

    #[test]
    fn random_edits_match_a_plain_string() {
        // A small deterministic generator; no rand dependency.
        let mut seed = 0x9E3779B97F4A7C15u64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n.max(1)
        };
        let original: String = (0..200).map(|i| format!("row {i}\n")).collect();
        let mut model = original.clone();
        let mut d = indexed(&original);
        for step in 0..300 {
            let len = model.len() as u64;
            if step % 3 == 0 && len > 0 {
                let off = next(len);
                let n = next((len - off).min(20)) + 1;
                // Keep to char boundaries: the text is ASCII.
                model.replace_range(off as usize..(off + n) as usize, "");
                d.delete(off, n).unwrap();
            } else {
                let off = next(len + 1);
                let text = ["x", "yy\n", "\n\n", "abc"][next(4) as usize];
                model.insert_str(off as usize, text);
                d.insert(off, text).unwrap();
            }
            assert_eq!(text_of(&d), model, "step {step}");
            let expect: Vec<_> = model.split('\n').map(str::to_owned).collect();
            assert_eq!(lines_of(&mut d), expect, "step {step}");
        }
    }

    #[test]
    fn saves_atomically_beside_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        fs::write(&path, "alpha\nbeta\n").unwrap();
        let mut d = Document::open(&path).unwrap();
        d.index_complete().unwrap();
        d.insert(6, "b is for ").unwrap();
        d.save_to(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nb is for beta\n");
        assert!(!d.is_modified());
        assert!(fs::read_dir(dir.path()).unwrap().count() == 1, "no temp file left");
        // The document still reads its original through the open handle.
        assert_eq!(d.line(1).unwrap().as_deref(), Some("b is for beta"));
    }
}
