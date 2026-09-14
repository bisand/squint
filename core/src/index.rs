//! A sparse line index over a [`Source`], lifted from ctail's tailer.
//!
//! The index keeps the byte offset of line 1 and of every `stride`-th line
//! after it, so it costs 8 bytes per thousand lines and finding line *n* is a
//! binary search plus a read of at most `stride` lines. It is built
//! incrementally: [`LineIndex::advance`] scans a bounded number of bytes, so a
//! front end can run the scan on a background thread in slices and show the
//! lines it has already passed while the count is still unknown.

use crate::source::{Source, after_nth_newline, count_newlines};
use std::io;

/// Lines per checkpoint. ctail uses the same figure.
pub const DEFAULT_STRIDE: u64 = 1000;

const SCAN_CHUNK: usize = 1024 * 1024;

pub struct LineIndex {
    stride: u64,
    /// Byte offset of line 0 and of every `stride`-th line after it.
    checkpoints: Vec<u64>,
    /// Length of the source the index describes.
    len: u64,
    /// Bytes scanned so far, from offset 0.
    scanned: u64,
    /// Newlines in `[0, scanned)`.
    newlines: u64,
}

impl LineIndex {
    pub fn new(len: u64, stride: u64) -> Self {
        Self {
            stride: stride.max(1),
            checkpoints: vec![0],
            len,
            scanned: 0,
            newlines: 0,
        }
    }

    /// An index built somewhere else: the length of what it describes, the
    /// offsets of line 0 and of every `stride`-th line after it, and how many
    /// newlines there are in all.
    ///
    /// For bytes that were counted as they were made — a projection formats
    /// every byte exactly once and sees the newlines go past — where scanning
    /// them the usual way would be a second pass over the whole document.
    pub fn from_checkpoints(len: u64, stride: u64, checkpoints: Vec<u64>, newlines: u64) -> Self {
        Self {
            stride: stride.max(1),
            checkpoints: if checkpoints.is_empty() {
                vec![0]
            } else {
                checkpoints
            },
            len,
            scanned: len,
            newlines,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.scanned >= self.len
    }

    /// Bytes scanned so far.
    pub fn scanned(&self) -> u64 {
        self.scanned
    }

    /// Newlines seen so far, in `[0, scanned)`.
    pub fn newlines(&self) -> u64 {
        self.newlines
    }

    /// Bytes the index holds.
    pub fn memory_bytes(&self) -> usize {
        self.checkpoints.capacity() * std::mem::size_of::<u64>()
    }

    /// Scans up to `budget` more bytes. Returns whether the index is complete.
    /// A source that turns out shorter than it said is treated as ending where
    /// the reads stop.
    pub fn advance(&mut self, source: &dyn Source, budget: usize) -> io::Result<bool> {
        let end = self.len.min(self.scanned.saturating_add(budget as u64));
        let mut buf = vec![0; SCAN_CHUNK.min(end.saturating_sub(self.scanned) as usize)];
        while self.scanned < end {
            let want = ((end - self.scanned) as usize).min(buf.len());
            let n = source.read_at(self.scanned, &mut buf[..want])?;
            if n == 0 {
                self.len = self.scanned;
                break;
            }
            for i in memchr::memchr_iter(b'\n', &buf[..n]) {
                self.newlines += 1;
                if self.newlines.is_multiple_of(self.stride) {
                    self.checkpoints.push(self.scanned + i as u64 + 1);
                }
            }
            self.scanned += n as u64;
        }
        Ok(self.is_complete())
    }

    /// Scans to the end.
    pub fn complete(&mut self, source: &dyn Source) -> io::Result<()> {
        while !self.advance(source, 4 * SCAN_CHUNK)? {}
        Ok(())
    }

    /// Newlines in `[0, offset)`, or `None` if `offset` lies beyond what has
    /// been scanned.
    pub fn newlines_before(&self, source: &dyn Source, offset: u64) -> io::Result<Option<u64>> {
        if offset > self.scanned {
            return Ok(None);
        }
        if offset == self.scanned {
            return Ok(Some(self.newlines));
        }
        let k = self.checkpoints.partition_point(|&c| c <= offset) - 1;
        let base = k as u64 * self.stride;
        Ok(Some(
            base + count_newlines(source, self.checkpoints[k], offset)?,
        ))
    }

    /// Newlines in `[from, to)`. `from` must lie within the scanned prefix;
    /// the part past it, if any, is read directly.
    pub fn newlines_in(&self, source: &dyn Source, from: u64, to: u64) -> io::Result<Option<u64>> {
        if to <= from {
            return Ok(Some(0));
        }
        let Some(a) = self.newlines_before(source, from)? else {
            return Ok(None);
        };
        if to <= self.scanned {
            let b = self.newlines_before(source, to)?.unwrap_or(self.newlines);
            return Ok(Some(b - a));
        }
        Ok(Some(
            self.newlines - a + count_newlines(source, self.scanned, to)?,
        ))
    }

    /// Byte offset where 0-based line `n` starts, or `None` if the scan has
    /// not reached it yet. Line `newlines()` is the one after the last newline
    /// seen; it starts within the scanned prefix even if it does not end there.
    pub fn line_start(&self, source: &dyn Source, n: u64) -> io::Result<Option<u64>> {
        if n > self.newlines {
            return Ok(None);
        }
        let k = (n / self.stride) as usize;
        let from = self.checkpoints[k];
        after_nth_newline(source, from, self.scanned, n - k as u64 * self.stride)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemSource;

    fn indexed(text: &str, stride: u64) -> (MemSource, LineIndex) {
        let s = MemSource(text.as_bytes().to_vec());
        let mut ix = LineIndex::new(s.len(), stride);
        ix.complete(&s).unwrap();
        (s, ix)
    }

    #[test]
    fn checkpoints_every_stride_lines() {
        let (s, ix) = indexed("a\nbb\nccc\ndddd\n", 2);
        assert_eq!(ix.checkpoints, vec![0, 5, 14]);
        assert_eq!(ix.newlines(), 4);
        let starts: Vec<_> = (0..=4).map(|n| ix.line_start(&s, n).unwrap()).collect();
        assert_eq!(starts, vec![Some(0), Some(2), Some(5), Some(9), Some(14)]);
        assert_eq!(ix.line_start(&s, 5).unwrap(), None);
    }

    #[test]
    fn counts_newlines_before_and_between() {
        let (s, ix) = indexed("a\nbb\nccc\ndddd\ne", 2);
        assert_eq!(ix.newlines_before(&s, 0).unwrap(), Some(0));
        assert_eq!(ix.newlines_before(&s, 2).unwrap(), Some(1));
        assert_eq!(ix.newlines_before(&s, 9).unwrap(), Some(3));
        assert_eq!(ix.newlines_before(&s, 15).unwrap(), Some(4));
        assert_eq!(ix.newlines_in(&s, 2, 14).unwrap(), Some(3));
        assert_eq!(ix.newlines_in(&s, 3, 3).unwrap(), Some(0));
    }

    #[test]
    fn partial_scan_answers_only_for_the_prefix() {
        let s = MemSource(b"a\nbb\nccc\ndddd\ne".to_vec());
        let mut ix = LineIndex::new(s.len(), 2);
        assert!(!ix.advance(&s, 6).unwrap());
        assert_eq!(ix.scanned(), 6);
        assert_eq!(ix.newlines(), 2);
        assert_eq!(ix.line_start(&s, 2).unwrap(), Some(5));
        assert_eq!(ix.line_start(&s, 3).unwrap(), None);
        assert_eq!(ix.newlines_before(&s, 7).unwrap(), None);
        // Past the prefix is read directly when the start is inside it.
        assert_eq!(ix.newlines_in(&s, 2, 14).unwrap(), Some(3));
        assert_eq!(ix.newlines_in(&s, 7, 14).unwrap(), None);
        assert!(ix.advance(&s, 100).unwrap());
        assert_eq!(ix.line_start(&s, 4).unwrap(), Some(14));
    }

    #[test]
    fn a_shrunken_source_ends_where_reads_stop() {
        let s = MemSource(b"a\nb\n".to_vec());
        let mut ix = LineIndex::new(100, 1000);
        assert!(ix.complete(&s).is_ok());
        assert!(ix.is_complete());
        assert_eq!(ix.newlines(), 2);
    }
}
