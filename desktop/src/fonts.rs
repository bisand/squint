//! Finds a monospace face for the text and a UI face for the chrome on the
//! machine we run on, falling back to Denise's built-in bitmap font. Lifted
//! from ctail, and since the settings dialog, able to load a face the settings
//! name: one of the faces squint looks for, or any TrueType file at all.

use denise_text::{GlyphSource, TrueTypeSource};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const DIRS: &[&str] = &[
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "/System/Library/Fonts",
    "/System/Library/Fonts/Supplemental",
    "/Library/Fonts",
    "C:\\Windows\\Fonts",
];

pub const MONO: &[&str] = &[
    "SFNSMono.ttf",
    "Menlo.ttf",
    "Monaco.ttf",
    "DejaVuSansMono.ttf",
    "LiberationMono-Regular.ttf",
    "JetBrainsMono-Regular.ttf",
    "consola.ttf",
    "Andale Mono.ttf",
    "cour.ttf",
];

pub const UI: &[&str] = &[
    "SFNS.ttf",
    "DejaVuSans.ttf",
    "LiberationSans-Regular.ttf",
    "NotoSans-Regular.ttf",
    "segoeui.ttf",
    "Arial.ttf",
    "Helvetica.ttf",
];

/// Every TrueType file in the places fonts live here, found once.
fn installed() -> &'static [PathBuf] {
    static FOUND: OnceLock<Vec<PathBuf>> = OnceLock::new();
    FOUND.get_or_init(|| {
        let mut found = Vec::new();
        for dir in DIRS {
            collect(Path::new(dir), 0, &mut found);
        }
        found
    })
}

pub fn load(preferred: &[&str]) -> Option<(String, Box<dyn GlyphSource>)> {
    let path = preferred.iter().find_map(|want| file_called(want))?;
    read(&path)
}

/// The face `name` asks for: a file, if it is a path to one, and otherwise the
/// face of that name among the ones installed — with or without the `.ttf`.
/// `None` when there is no such face, so the caller can fall back to the list
/// it would have used.
pub fn load_named(name: &str) -> Option<(String, Box<dyn GlyphSource>)> {
    let as_file = Path::new(name);
    if as_file.is_file() {
        return read(as_file);
    }
    read(&file_called(name)?)
}

/// The faces of `preferred` this machine has, by the name the settings name
/// them: the file's name without `.ttf`.
pub fn choices(preferred: &[&str]) -> Vec<String> {
    preferred
        .iter()
        .filter(|want| file_called(want).is_some())
        .map(|want| stem_of(want))
        .collect()
}

fn stem_of(file: &str) -> String {
    Path::new(file)
        .file_stem()
        .map_or_else(|| file.to_string(), |s| s.to_string_lossy().into_owned())
}

/// The installed file called `want`, with or without its extension.
fn file_called(want: &str) -> Option<PathBuf> {
    let want_stem = stem_of(want);
    installed()
        .iter()
        .find(|p| {
            p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(want))
                || p.file_stem()
                    .is_some_and(|n| n.eq_ignore_ascii_case(want_stem.as_str()))
        })
        .cloned()
}

fn read(path: &Path) -> Option<(String, Box<dyn GlyphSource>)> {
    let name = path.display().to_string();
    let bytes = std::fs::read(path).ok()?;
    let source = TrueTypeSource::from_bytes(&name, &bytes).ok()?;
    Some((name, Box::new(source)))
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, depth + 1, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("ttf"))
        {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A face is named by its file's stem, whichever way it is asked for.
    #[test]
    fn a_face_is_named_by_its_file() {
        assert_eq!(stem_of("SFNSMono.ttf"), "SFNSMono");
        assert_eq!(stem_of("Andale Mono.ttf"), "Andale Mono");
    }

    /// Every face offered is one that can then be loaded.
    #[test]
    fn every_choice_can_be_loaded() {
        for name in choices(MONO) {
            assert!(load_named(&name).is_some(), "{name}");
        }
    }
}
