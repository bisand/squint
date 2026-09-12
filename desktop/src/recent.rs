//! The files opened lately, for File ▸ Open Recent, kept between runs as one
//! path a line in the user's configuration directory.

use std::fs;
use std::path::{Path, PathBuf};

/// How many are kept, until the settings say otherwise.
const KEEP: usize = 10;

pub struct Recent {
    paths: Vec<PathBuf>,
    /// How many are kept: the settings' doing. None kept is a list turned off,
    /// which is emptied rather than left where somebody could still read it.
    keep: usize,
    /// Where the list is kept, or `None` for a list that is never written.
    file: Option<PathBuf>,
}

impl Recent {
    /// The list as the last run left it.
    pub fn load() -> Self {
        let file = dirs::config_dir().map(|dir| dir.join("squint").join("recent"));
        let paths = file
            .as_deref()
            .and_then(|f| fs::read_to_string(f).ok())
            .map(|text| {
                text.lines()
                    .filter(|line| !line.is_empty())
                    .map(PathBuf::from)
                    .take(KEEP)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            paths,
            keep: KEEP,
            file,
        }
    }

    /// An empty list that is never written: for a snapshot, which should leave
    /// the user's alone.
    pub fn in_memory() -> Self {
        Self {
            paths: Vec::new(),
            keep: KEEP,
            file: None,
        }
    }

    /// Keeps only so many from now on, and drops any past that.
    pub fn set_limit(&mut self, keep: usize) {
        if self.keep == keep {
            return;
        }
        self.keep = keep;
        if self.paths.len() > keep {
            self.paths.truncate(keep);
            self.store();
        }
    }

    /// Most recent first.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Puts `path` at the top, once. A list of none keeps nothing at all.
    pub fn add(&mut self, path: &Path) {
        if self.keep == 0 {
            return;
        }
        self.paths.retain(|p| p != path);
        self.paths.insert(0, path.to_path_buf());
        self.paths.truncate(self.keep);
        self.store();
    }

    pub fn remove(&mut self, path: &Path) {
        let before = self.paths.len();
        self.paths.retain(|p| p != path);
        if self.paths.len() != before {
            self.store();
        }
    }

    pub fn clear(&mut self) {
        self.paths.clear();
        self.store();
    }

    /// Writes the list, quietly: a list that cannot be kept is not worth
    /// interrupting anybody over.
    fn store(&self) {
        let Some(file) = &self.file else {
            return;
        };
        if let Some(dir) = file.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let mut text = String::new();
        for path in &self.paths {
            text.push_str(&path.to_string_lossy());
            text.push('\n');
        }
        let _ = fs::write(file, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_is_first_and_each_file_is_there_once() {
        let mut recent = Recent::in_memory();
        recent.add(Path::new("/a"));
        recent.add(Path::new("/b"));
        recent.add(Path::new("/a"));
        assert_eq!(recent.paths(), [PathBuf::from("/a"), PathBuf::from("/b")]);
        recent.remove(Path::new("/a"));
        assert_eq!(recent.paths(), [PathBuf::from("/b")]);
    }

    #[test]
    fn only_so_many_are_kept() {
        let mut recent = Recent::in_memory();
        for n in 0..KEEP + 5 {
            recent.add(&PathBuf::from(format!("/{n}")));
        }
        assert_eq!(recent.paths().len(), KEEP);
        assert_eq!(recent.paths()[0], PathBuf::from(format!("/{}", KEEP + 4)));
    }
}
