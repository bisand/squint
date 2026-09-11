//! Whether a file has been changed by something else since squint last read
//! or wrote it.
//!
//! A document reads its file where it is looked at and never holds all of it,
//! so a file changed underneath is not merely out of date: what is on screen
//! can become a mix of the old file and the new. Noticing matters.
//!
//! The stamp is the size, the modification time and, where there is one, the
//! inode — which is what changes when a file is replaced by renaming another
//! over it, the way squint saves and so do most editors. Looking is one `stat`
//! a file, cheap enough to do every second or two for every tab.

use std::fs::{self, Metadata};
use std::path::Path;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    file: u64,
}

impl Stamp {
    /// The file at `path` as it is now, or `None` when it cannot be looked at:
    /// gone, or not allowed.
    pub fn of(path: &Path) -> Option<Self> {
        let meta = fs::metadata(path).ok()?;
        Some(Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            file: file_id(&meta),
        })
    }
}

#[cfg(unix)]
fn file_id(meta: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino()
}

#[cfg(not(unix))]
fn file_id(_meta: &Metadata) -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_file_looks_the_same_until_it_is_changed() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("server.log");
        fs::write(&path, "one\n").expect("write");
        let first = Stamp::of(&path).expect("stamp");
        assert_eq!(Stamp::of(&path), Some(first));

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open");
        file.write_all(b"two\n").expect("append");
        drop(file);
        assert_ne!(Stamp::of(&path), Some(first), "grown");

        fs::remove_file(&path).expect("remove");
        assert_eq!(Stamp::of(&path), None, "gone");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_replaced_by_one_the_same_size_is_a_change() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("a.txt");
        let other = dir.path().join("b.txt");
        fs::write(&path, "abc").expect("write");
        let first = Stamp::of(&path).expect("stamp");
        fs::write(&other, "xyz").expect("write");
        fs::rename(&other, &path).expect("rename");
        assert_ne!(Stamp::of(&path), Some(first));
    }
}
