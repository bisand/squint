//! A source's formatted form, measured but never written.
//!
//! ⇧⌘F formats by streaming the document to a file and opening that. It is
//! one pass and flat memory, but for a minified document it writes a copy
//! half again as big as the original — eight gigabytes for a four-gigabyte
//! JSON — and nobody wanted the copy. They wanted to read the file.
//!
//! A projection is the same formatter over the same source with the output
//! thrown away. What it keeps instead is small:
//!
//! - **Where the formatter can be picked up again.** Every [`MARK_STRIDE`]
//!   bytes of output, at the next point the formatter is holding nothing
//!   back, a copy of it is kept along with how far it had read and written.
//!   A formatted byte anywhere in the document is then reached by resuming
//!   from the mark below it and formatting forward — at most `MARK_STRIDE`
//!   of work, whatever the document weighs.
//! - **The line index, built as the bytes go by.** The scan sees every
//!   formatted byte exactly once, so it counts the newlines and keeps the
//!   offset of every thousandth line on the way past. A document over the
//!   projection is handed that index rather than reading the whole thing
//!   again to build one.
//!
//! What comes out is a [`Projected`], which is a [`Source`] like any other:
//! the piece table, the line index, find, the highlighting and saving all
//! work over it without knowing it is not a file. Saving streams it to disk,
//! which is the write that was never done.
//!
//! The scan is one pass over the source and writes nothing. It is not free —
//! the length and the line count are not knowable without it — but it is half
//! the work of formatting to a file and none of the disk.

use crate::document::Document;
use crate::format::{self, Formatter, Kind, Style};
use crate::index::{DEFAULT_STRIDE, LineIndex};
use crate::source::Source;
use std::io;
use std::sync::{Arc, Mutex};

/// Formatted bytes between the marks a projection keeps.
///
/// It is what a read that lands anywhere may have to format to get there, and
/// what a mark costs is a copy of the formatter, so this trades a fraction of
/// a millisecond per seek against a few megabytes on the largest documents.
const MARK_STRIDE: u64 = 256 * 1024;

/// Source bytes fed to the formatter at a time.
const CHUNK: usize = 256 * 1024;

/// The formatter at a formatted offset, ready to go on from there.
struct Mark {
    /// Source bytes it had read.
    read: u64,
    /// Formatted bytes it had written.
    wrote: u64,
    at: Box<dyn Formatter + Send + Sync>,
}

/// A scan of a source through a formatter, a bounded step at a time.
///
/// Drive [`advance`](Projection::advance) until it says it is finished, then
/// [`finish`](Projection::finish) for the source and index to open a document
/// over.
pub struct Projection {
    source: Arc<dyn Source>,
    kind: Kind,
    style: Style,
    at: Box<dyn Formatter + Send + Sync>,
    marks: Vec<Mark>,
    /// Formatted offset of line 0 and of every `DEFAULT_STRIDE`-th line.
    lines: Vec<u64>,
    newlines: u64,
    /// Source bytes read.
    read: u64,
    /// Formatted bytes written.
    wrote: u64,
    /// Formatted bytes written when the last mark was taken.
    marked: u64,
    done: bool,
    input: Vec<u8>,
    output: Vec<u8>,
}

impl Projection {
    pub fn new(source: Arc<dyn Source>, kind: Kind, style: Style) -> Self {
        Self {
            source,
            kind,
            style,
            at: format::formatter(kind, style),
            marks: Vec::new(),
            lines: vec![0],
            newlines: 0,
            read: 0,
            wrote: 0,
            marked: 0,
            done: false,
            input: Vec::new(),
            output: Vec::new(),
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Source bytes read so far, out of the source's length.
    pub fn progress(&self) -> (u64, u64) {
        (self.read.min(self.source.len()), self.source.len())
    }

    pub fn is_complete(&self) -> bool {
        self.done
    }

    /// Formatted bytes so far — the whole of it once the scan has finished.
    pub fn len(&self) -> u64 {
        self.wrote
    }

    pub fn is_empty(&self) -> bool {
        self.wrote == 0
    }

    /// Bytes the marks and the line checkpoints hold. The output is not among
    /// them: it is counted and dropped.
    pub fn memory_bytes(&self) -> usize {
        self.marks.len() * std::mem::size_of::<Mark>()
            + self.lines.capacity() * std::mem::size_of::<u64>()
            + self.input.capacity()
            + self.output.capacity()
    }

    /// Formats up to `budget` more bytes of the source, counting what comes
    /// out rather than keeping it. Returns whether the whole source has been
    /// through. Call it from a worker, or in slices between frames.
    pub fn advance(&mut self, budget: usize) -> io::Result<bool> {
        if self.done {
            return Ok(true);
        }
        let mut left = budget.max(1);
        let mut output = std::mem::take(&mut self.output);
        while !self.done && left > 0 {
            output.clear();
            self.input.resize(left.min(CHUNK), 0);
            let n = self.source.read_at(self.read, &mut self.input)?;
            if n == 0 {
                self.at.finish(&mut output);
                self.done = true;
            } else {
                self.at.feed(&self.input[..n], &mut output);
                self.read += n as u64;
                left = left.saturating_sub(n);
            }
            self.count(&output);
        }
        self.output = output;
        Ok(self.done)
    }

    /// Counts what one feed produced, and marks where the formatter is if it
    /// is far enough past the last mark and holding nothing back.
    fn count(&mut self, output: &[u8]) {
        for i in memchr::memchr_iter(b'\n', output) {
            self.newlines += 1;
            if self.newlines.is_multiple_of(DEFAULT_STRIDE) {
                self.lines.push(self.wrote + i as u64 + 1);
            }
        }
        self.wrote += output.len() as u64;
        if self.wrote - self.marked >= MARK_STRIDE && self.at.at_rest() {
            self.marked = self.wrote;
            self.marks.push(Mark {
                read: self.read,
                wrote: self.wrote,
                at: self.at.copy(),
            });
        }
    }

    /// The formatted bytes as a source, with the index the scan built for
    /// them. `None` until the scan has been all the way through: until then
    /// neither the length nor the line count is known, and a document is
    /// both.
    pub fn finish(self) -> Option<(Projected, LineIndex)> {
        if !self.done {
            return None;
        }
        let index =
            LineIndex::from_checkpoints(self.wrote, DEFAULT_STRIDE, self.lines, self.newlines);
        let scan = Arc::new(Scan {
            source: self.source,
            kind: self.kind,
            style: self.style,
            marks: self.marks,
            len: self.wrote,
        });
        let cursor = Mutex::new(Cursor::new(&scan));
        Some((Projected { scan, cursor }, index))
    }

    /// A document over the formatted bytes, once the scan has finished.
    pub fn into_document(self) -> Option<Document> {
        let (projected, index) = self.finish()?;
        Some(Document::from_indexed_source(Box::new(projected), index))
    }
}

/// What a finished scan leaves to read the formatted bytes with.
struct Scan {
    source: Arc<dyn Source>,
    kind: Kind,
    style: Style,
    marks: Vec<Mark>,
    len: u64,
}

impl Scan {
    /// The mark to pick the formatter up from for `off`: the last one at or
    /// before it, if any.
    fn mark_for(&self, off: u64) -> Option<usize> {
        self.marks
            .partition_point(|m| m.wrote <= off)
            .checked_sub(1)
    }

    /// The formatted offset that mark sits at, or the start of the document
    /// where there is no mark below `off`.
    fn mark_at(&self, off: u64) -> u64 {
        self.mark_for(off).map_or(0, |i| self.marks[i].wrote)
    }
}

/// A formatter somewhere in the document, and the last of what it produced.
///
/// `pending` holds the formatted bytes from `base` on, and `taken` is how far
/// into them the reading has got: the bytes before it are kept rather than
/// dropped, so a read that goes back a little — every line read goes back to
/// the start of the line it just found the end of — is a move within the
/// buffer instead of formatting from a mark again. Past the end of `pending`,
/// more comes from feeding the formatter more source.
struct Cursor {
    at: Box<dyn Formatter + Send + Sync>,
    read: u64,
    /// Formatted offset of `pending[0]`.
    base: u64,
    pending: Vec<u8>,
    taken: usize,
    /// The formatter has been finished: nothing more will come out of it.
    ended: bool,
    input: Vec<u8>,
}

impl Cursor {
    fn new(scan: &Scan) -> Self {
        Self {
            at: format::formatter(scan.kind, scan.style),
            read: 0,
            base: 0,
            pending: Vec::new(),
            taken: 0,
            ended: false,
            input: Vec::new(),
        }
    }

    /// The formatted offset the reading is at.
    fn at(&self) -> u64 {
        self.base + self.taken as u64
    }

    /// The formatted bytes at [`at`](Self::at) that are already in hand.
    fn ready(&self) -> &[u8] {
        &self.pending[self.taken..]
    }

    fn eat(&mut self, n: usize) {
        self.taken += n;
    }

    /// Whether `off` is in what the cursor is still holding, going back as
    /// well as forward.
    fn holds(&self, off: u64) -> bool {
        off >= self.base && off < self.base + self.pending.len() as u64
    }

    /// Moves within what is held. Only where [`holds`](Self::holds) says so.
    fn jump(&mut self, off: u64) {
        self.taken = (off - self.base) as usize;
    }

    /// Picks the formatter up again at or before `off`: the mark below it, or
    /// the start of the document where there is none.
    fn seek(&mut self, scan: &Scan, off: u64) {
        match scan.mark_for(off) {
            Some(i) => {
                let mark = &scan.marks[i];
                self.at = mark.at.copy();
                self.read = mark.read;
                self.base = mark.wrote;
            }
            None => {
                self.at = format::formatter(scan.kind, scan.style);
                self.read = 0;
                self.base = 0;
            }
        }
        self.pending.clear();
        self.taken = 0;
        self.ended = false;
    }

    /// Formats on until there is something at [`at`](Self::at). Whether there
    /// is: `false` is the end of the document.
    fn produce(&mut self, scan: &Scan) -> io::Result<bool> {
        while self.taken >= self.pending.len() {
            if self.ended {
                return Ok(false);
            }
            self.base += self.pending.len() as u64;
            self.pending.clear();
            self.taken = 0;
            self.input.resize(CHUNK, 0);
            let n = scan.source.read_at(self.read, &mut self.input)?;
            if n == 0 {
                self.at.finish(&mut self.pending);
                self.ended = true;
            } else {
                self.at.feed(&self.input[..n], &mut self.pending);
                self.read += n as u64;
            }
        }
        Ok(true)
    }
}

/// The formatted bytes of a source, read where they are looked at.
///
/// Reading forward costs the formatting alone. Reading somewhere else costs
/// that plus up to [`MARK_STRIDE`] of formatting to get there, which is what
/// the marks are for.
pub struct Projected {
    scan: Arc<Scan>,
    cursor: Mutex<Cursor>,
}

impl Projected {
    /// What the formatted bytes were made from.
    pub fn source(&self) -> &Arc<dyn Source> {
        &self.scan.source
    }

    pub fn kind(&self) -> Kind {
        self.scan.kind
    }

    /// Bytes the marks hold.
    pub fn memory_bytes(&self) -> usize {
        self.scan.marks.len() * std::mem::size_of::<Mark>()
    }
}

impl Source for Projected {
    fn len(&self) -> u64 {
        self.scan.len
    }

    fn memory_bytes(&self) -> usize {
        self.scan.marks.len() * std::mem::size_of::<Mark>() + self.scan.source.memory_bytes()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || offset >= self.scan.len {
            return Ok(0);
        }
        let scan = &self.scan;
        let mut cursor = self.cursor.lock().unwrap_or_else(|e| e.into_inner());
        // Back to the mark below `offset` when the cursor is past it, and
        // forward to it when the cursor is a long way short: formatting on
        // from where it happens to be would be the whole document, for a
        // scroll to the end of one.
        if cursor.holds(offset) {
            cursor.jump(offset);
        } else if cursor.at() > offset || scan.mark_at(offset) > cursor.at() {
            cursor.seek(scan, offset);
        }
        while cursor.at() < offset {
            if !cursor.produce(scan)? {
                return Ok(0);
            }
            let skip = ((offset - cursor.at()) as usize).min(cursor.ready().len());
            cursor.eat(skip);
        }
        let mut filled = 0;
        while filled < buf.len() {
            if !cursor.produce(scan)? {
                break;
            }
            let n = cursor.ready().len().min(buf.len() - filled);
            buf[filled..filled + n].copy_from_slice(&cursor.ready()[..n]);
            cursor.eat(n);
            filled += n;
        }
        Ok(filled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::format_document;
    use crate::source::MemSource;

    fn formatted(text: &str, kind: Kind) -> Vec<u8> {
        let doc = Document::from_text(text);
        let mut out = Vec::new();
        format_document(&doc, kind, Style::default(), &mut out).unwrap();
        out
    }

    fn project(text: &str, kind: Kind, budget: usize) -> (Projected, LineIndex) {
        let source = Arc::new(MemSource(text.as_bytes().to_vec()));
        let mut scan = Projection::new(source, kind, Style::default());
        while !scan.advance(budget).unwrap() {}
        scan.finish().expect("finished")
    }

    fn json(n: usize) -> String {
        let rows: Vec<String> = (0..n)
            .map(|i| {
                format!(
                    r#"{{"id":{i},"name":"row-{i}","tags":["a","b"],"ok":{}}}"#,
                    i % 2 == 0
                )
            })
            .collect();
        format!("[{}]", rows.join(","))
    }

    #[test]
    fn a_projection_reads_as_the_formatted_file_would() {
        let text = json(400);
        let want = formatted(&text, Kind::Json);
        for budget in [7, 1000, 1 << 20] {
            let (p, _) = project(&text, Kind::Json, budget);
            assert_eq!(p.len(), want.len() as u64, "budget {budget}");
            let mut got = vec![0; want.len()];
            let n = p.read_at(0, &mut got).unwrap();
            assert_eq!(n, want.len());
            assert_eq!(got, want, "read whole, budget {budget}");
        }
    }

    #[test]
    fn reads_anywhere_match_reads_from_the_start() {
        let text = json(3000);
        let want = formatted(&text, Kind::Json);
        let (p, _) = project(&text, Kind::Json, 1 << 16);
        assert!(
            p.memory_bytes() > 0 && want.len() as u64 > MARK_STRIDE,
            "big enough to have marks"
        );

        // Backwards, forwards, straddling marks, past the end.
        let len = want.len() as u64;
        let spots = [
            len - 1,
            0,
            len / 2,
            MARK_STRIDE - 3,
            MARK_STRIDE + 3,
            len / 3,
            len - 10,
            17,
            len,
            len + 100,
        ];
        for off in spots {
            let mut got = vec![0; 700];
            let n = p.read_at(off, &mut got).unwrap();
            let from = (off as usize).min(want.len());
            let to = (from + 700).min(want.len());
            assert_eq!(n, to - from, "at {off}");
            assert_eq!(&got[..n], &want[from..to], "at {off}");
        }
    }

    #[test]
    fn the_index_the_scan_built_finds_the_same_lines() {
        let text = json(3000);
        let want = formatted(&text, Kind::Json);
        let (p, index) = project(&text, Kind::Json, 1 << 16);
        let lines: Vec<&[u8]> = want.split(|b| *b == b'\n').collect();
        // The formatter ends with a newline, so the last split is empty.
        assert!(
            index.newlines() > DEFAULT_STRIDE,
            "more than one checkpoint"
        );
        assert_eq!(index.newlines(), lines.len() as u64 - 1);

        let mut starts = vec![0u64];
        for (i, b) in want.iter().enumerate() {
            if *b == b'\n' {
                starts.push(i as u64 + 1);
            }
        }
        for n in [0, 1, 999, 1000, 1001, 2500, index.newlines() - 1] {
            assert_eq!(
                index.line_start(&p, n).unwrap(),
                Some(starts[n as usize]),
                "line {n}"
            );
        }
    }

    #[test]
    fn a_document_over_a_projection_reads_its_lines() {
        let text = json(2000);
        let want = formatted(&text, Kind::Json);
        let source = Arc::new(MemSource(text.as_bytes().to_vec()));
        let mut scan = Projection::new(source, Kind::Json, Style::default());
        while !scan.advance(1 << 16).unwrap() {}
        let mut doc = scan.into_document().expect("finished");

        let lines: Vec<String> = String::from_utf8(want.clone())
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert!(doc.is_indexed(), "the scan built the index as it went");
        assert_eq!(doc.line_count().unwrap(), Some(lines.len() as u64 + 1));
        for n in [0, 1, 2, 1000, 5000, lines.len() as u64 - 1] {
            assert_eq!(
                doc.line(n).unwrap().as_deref(),
                Some(lines[n as usize].as_str())
            );
        }

        // And it saves as the formatted file: what was never written.
        let mut out = Vec::new();
        doc.write_to(&mut out).unwrap();
        assert_eq!(out, want);
    }

    #[test]
    fn a_projection_of_xml_reads_as_its_formatted_file_would() {
        let mut text = String::from("<feed>");
        for i in 0..2000 {
            text.push_str(&format!("<item id='{i}'><name>row {i}</name><ok/></item>"));
        }
        text.push_str("</feed>");
        let want = formatted(&text, Kind::Xml);
        let (p, _) = project(&text, Kind::Xml, 1 << 14);
        assert_eq!(p.len(), want.len() as u64);
        for off in [0u64, 5000, want.len() as u64 / 2, 1] {
            let mut got = vec![0; 4096];
            let n = p.read_at(off, &mut got).unwrap();
            let from = off as usize;
            let to = (from + 4096).min(want.len());
            assert_eq!(&got[..n], &want[from..to], "at {off}");
        }
    }

    /// Counts what is read through it, to tell a read that went to the mark
    /// below what it wanted from one that formatted its way there.
    struct Counted {
        inner: MemSource,
        read: std::sync::atomic::AtomicU64,
    }

    impl Source for Counted {
        fn len(&self) -> u64 {
            self.inner.len()
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.inner.read_at(offset, buf)?;
            self.read
                .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
            Ok(n)
        }
    }

    #[test]
    fn a_read_anywhere_costs_a_mark_and_not_the_document() {
        let text = json(60000);
        let source = Arc::new(Counted {
            inner: MemSource(text.clone().into_bytes()),
            read: Default::default(),
        });
        let counter = source.clone();
        let mut scan = Projection::new(source, Kind::Json, Style::default());
        while !scan.advance(1 << 16).unwrap() {}
        let (p, _) = scan.finish().expect("finished");
        assert!(
            p.len() > 8 * MARK_STRIDE,
            "plenty of marks to choose from, not {}",
            p.len()
        );

        // Forward to the far end, then back to the near one, then the far end
        // again: each is a mark away, never the document.
        let read = |at: u64| {
            counter.read.store(0, std::sync::atomic::Ordering::Relaxed);
            let mut buf = vec![0; 64];
            p.read_at(at, &mut buf).unwrap();
            counter.read.load(std::sync::atomic::Ordering::Relaxed)
        };
        let far = p.len() - 100;
        for at in [far, 500, far, p.len() / 2] {
            let cost = read(at);
            assert!(
                cost <= 2 * CHUNK as u64,
                "reading at {at} read {cost} source bytes, which is more than a mark away"
            );
        }
    }

    #[test]
    fn a_scan_that_has_not_finished_has_no_document_yet() {
        let source = Arc::new(MemSource(json(500).into_bytes()));
        let mut scan = Projection::new(source, Kind::Json, Style::default());
        assert!(!scan.advance(16).unwrap());
        assert!(!scan.is_complete());
        let (read, total) = scan.progress();
        assert!(read > 0 && read < total);
        assert!(scan.into_document().is_none());
    }
}
