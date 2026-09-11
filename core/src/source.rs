//! Where a document's original bytes come from.
//!
//! A [`Source`] is random-access and read-only. The file-backed one reads with
//! `pread`, so opening a document costs one `open` and one `stat`: nothing is
//! loaded that is not looked at. The in-memory one serves tests and pasted
//! text.

use std::fs::File;
use std::io;
use std::path::Path;

/// Random-access, read-only bytes.
pub trait Source: Send + Sync {
    /// Total length in bytes.
    fn len(&self) -> u64;

    /// Reads at most `buf.len()` bytes starting at `offset` and returns how
    /// many were read. A short count means the end was reached, never that
    /// the source is busy: implementations retry on interruption themselves.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reads `[offset, offset + len)` clipped to the source.
    fn read_vec(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut out = vec![0; len];
        let n = self.read_at(offset, &mut out)?;
        out.truncate(n);
        Ok(out)
    }
}

/// A file on disk, read with positional reads so no cursor is shared.
///
/// The length is taken once, when the source is opened: a document is a
/// snapshot of the file as it was then, and a file that grows or shrinks
/// underneath is the caller's conflict to detect and resolve.
pub struct FileSource {
    file: File,
    len: u64,
}

impl FileSource {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file, len })
    }
}

impl Source for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let end = self.len.min(offset.saturating_add(buf.len() as u64));
        let want = end.saturating_sub(offset) as usize;
        let buf = &mut buf[..want];
        let mut filled = 0;
        while filled < buf.len() {
            match pread(&self.file, offset + filled as u64, &mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(filled)
    }
}

#[cfg(unix)]
fn pread(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buf, offset)
}

#[cfg(windows)]
fn pread(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buf, offset)
}

/// Bytes held in memory.
pub struct MemSource(pub Vec<u8>);

impl Source for MemSource {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let start = (offset as usize).min(self.0.len());
        let end = (start + buf.len()).min(self.0.len());
        buf[..end - start].copy_from_slice(&self.0[start..end]);
        Ok(end - start)
    }
}

/// Reads `[from, to)` of `source` in chunks and counts its newlines.
pub(crate) fn count_newlines(source: &dyn Source, from: u64, to: u64) -> io::Result<u64> {
    const CHUNK: usize = 256 * 1024;
    let mut buf = vec![0; CHUNK.min(to.saturating_sub(from) as usize)];
    let mut off = from;
    let mut count = 0;
    while off < to {
        let want = ((to - off) as usize).min(buf.len());
        let n = source.read_at(off, &mut buf[..want])?;
        if n == 0 {
            break;
        }
        count += bytecount::count(&buf[..n], b'\n') as u64;
        off += n as u64;
    }
    Ok(count)
}

/// Returns the offset just past the `n`-th newline (1-based) at or after
/// `from`, looking no further than `to`, or `None` if there are fewer.
pub(crate) fn after_nth_newline(
    source: &dyn Source,
    from: u64,
    to: u64,
    mut n: u64,
) -> io::Result<Option<u64>> {
    const CHUNK: usize = 256 * 1024;
    if n == 0 {
        return Ok(Some(from));
    }
    let mut buf = vec![0; CHUNK.min(to.saturating_sub(from) as usize)];
    let mut off = from;
    while off < to {
        let want = ((to - off) as usize).min(buf.len());
        let read = source.read_at(off, &mut buf[..want])?;
        if read == 0 {
            break;
        }
        for i in memchr::memchr_iter(b'\n', &buf[..read]) {
            n -= 1;
            if n == 0 {
                return Ok(Some(off + i as u64 + 1));
            }
        }
        off += read as u64;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_source_clips_reads() {
        let s = MemSource(b"hello".to_vec());
        let mut buf = [0; 3];
        assert_eq!(s.read_at(3, &mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"lo");
        assert_eq!(s.read_at(9, &mut buf).unwrap(), 0);
        assert_eq!(s.read_vec(1, 2).unwrap(), b"el");
    }

    #[test]
    fn file_source_reads_positionally() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"one\ntwo\nthree\n").unwrap();
        let s = FileSource::open(&path).unwrap();
        assert_eq!(s.len(), 14);
        assert_eq!(s.read_vec(4, 3).unwrap(), b"two");
        assert_eq!(s.read_vec(12, 100).unwrap(), b"e\n");
        assert_eq!(count_newlines(&s, 0, 14).unwrap(), 3);
        assert_eq!(after_nth_newline(&s, 0, 14, 2).unwrap(), Some(8));
        assert_eq!(after_nth_newline(&s, 0, 14, 4).unwrap(), None);
    }
}
