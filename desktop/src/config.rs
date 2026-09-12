//! What squint keeps between runs — the settings, the tabs that were open — as
//! JSON files in the user's configuration directory.
//!
//! Reading is forgiving and writing is quiet: a file that is missing, or that
//! holds something this squint cannot read, comes back as the defaults, and a
//! file that cannot be written is not worth interrupting anybody over.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs;
use std::path::{Path, PathBuf};

/// The file called `name` in squint's configuration directory.
pub fn file(name: &str) -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("squint").join(name))
}

/// A value kept in a file: read once, written whenever it changes.
pub struct Kept<T> {
    value: T,
    /// Where it is kept, or `None` for a value that is never written.
    file: Option<PathBuf>,
}

impl<T: Serialize + DeserializeOwned + Default> Kept<T> {
    /// The value the file called `name` holds, or the defaults.
    pub fn load(name: &str) -> Self {
        Self::load_from(file(name))
    }

    /// The value `file` holds, or the defaults.
    pub fn load_from(file: Option<PathBuf>) -> Self {
        let value = file
            .as_deref()
            .and_then(|f| fs::read(f).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { value, file }
    }

    /// A value that is never written: for a snapshot, which should leave the
    /// user's files alone.
    pub fn in_memory(value: T) -> Self {
        Self { value, file: None }
    }

    pub fn get(&self) -> &T {
        &self.value
    }

    /// Where the value is kept, for something that has to say so — or `None`
    /// for a value that is never written.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Changes the value and writes it.
    pub fn update(&mut self, change: impl FnOnce(&mut T)) {
        change(&mut self.value);
        self.store();
    }

    /// Replaces the value and writes it.
    pub fn set(&mut self, value: T) {
        self.value = value;
        self.store();
    }

    /// Writes the value as it is: for a file somebody is about to be shown,
    /// which should be there even if nothing has changed.
    pub fn save(&self) {
        self.store();
    }

    /// Writes the value whole: to a file beside the real one, renamed over it,
    /// so a crash leaves the old file rather than half of the new one.
    fn store(&self) {
        let Some(file) = &self.file else {
            return;
        };
        let Ok(json) = serde_json::to_vec_pretty(&self.value) else {
            return;
        };
        if let Some(dir) = file.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let part = part_of(file);
        if fs::write(&part, json)
            .and_then(|()| fs::rename(&part, file))
            .is_err()
        {
            let _ = fs::remove_file(&part);
        }
    }
}

fn part_of(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct Choice {
        on: bool,
        name: String,
    }

    #[test]
    fn a_kept_value_comes_back_the_next_time() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("nested").join("choice.json");
        let mut kept: Kept<Choice> = Kept::load_from(Some(file.clone()));
        assert_eq!(*kept.get(), Choice::default(), "nothing there yet");
        kept.update(|c| {
            c.on = true;
            c.name = "ø".into();
        });
        let again: Kept<Choice> = Kept::load_from(Some(file.clone()));
        assert_eq!(
            *again.get(),
            Choice {
                on: true,
                name: "ø".into()
            }
        );
        assert!(!part_of(&file).exists(), "nothing left beside it");
    }

    #[test]
    fn a_file_that_cannot_be_read_is_the_defaults() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("choice.json");
        fs::write(&file, "{ not json").expect("write");
        let kept: Kept<Choice> = Kept::load_from(Some(file));
        assert_eq!(*kept.get(), Choice::default());
    }
}
