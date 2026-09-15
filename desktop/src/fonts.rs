//! Finds a monospace face for the text and a UI face for the chrome on the
//! machine we run on, falling back to Denise's built-in bitmap font. Lifted
//! from ctail, and since the settings window, able to load a face the settings
//! name — one of the faces squint looks for, one of the faces installed, or
//! any font file at all — and to list every face there is, for the settings to
//! choose from.

use denise_text::{GlyphSource, TrueTypeSource};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where fonts live for everybody on the machine.
const DIRS: &[&str] = &[
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "/System/Library/Fonts",
    "/System/Library/Fonts/Supplemental",
    "/Library/Fonts",
    "C:\\Windows\\Fonts",
];

/// And where they live for this user, which is where a face somebody has
/// installed themselves is — a Nerd Font, most of the time.
fn user_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(fonts) = dirs::font_dir() {
        dirs.push(fonts);
    }
    if let Some(home) = dirs::home_dir() {
        // Where fontconfig looked before `~/.local/share/fonts`, and where
        // plenty of Linux machines still keep them.
        dirs.push(home.join(".fonts"));
    }
    if let Some(local) = dirs::data_local_dir() {
        // Windows, for a font installed without an administrator.
        dirs.push(local.join("Microsoft").join("Windows").join("Fonts"));
    }
    dirs
}

pub const MONO: &[&str] = &[
    // Fira Code's Nerd Font build first, where somebody has installed one: the
    // Mono variant of it, whose glyphs keep the grid the editor draws on — the
    // Propo one does not, and is deliberately not here. Both spellings the
    // Nerd Fonts project has used are listed, and plain Fira Code after them.
    "FiraCodeNerdFontMono-Regular.ttf",
    "FiraCodeNerdFont-Regular.ttf",
    "Fira Code Regular Nerd Font Complete Mono.ttf",
    "Fira Code Regular Nerd Font Complete.ttf",
    "FiraCode-Regular.ttf",
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

/// Every font file in the places fonts live here, found once: the machine's
/// and this user's.
fn installed() -> &'static [PathBuf] {
    static FOUND: OnceLock<Vec<PathBuf>> = OnceLock::new();
    FOUND.get_or_init(|| {
        let mut found = Vec::new();
        for dir in DIRS {
            collect(Path::new(dir), 0, &mut found);
        }
        for dir in user_dirs() {
            collect(&dir, 0, &mut found);
        }
        found.sort();
        found.dedup();
        found
    })
}

/// Every face installed, by the name the settings name it, in order and once
/// each. What the settings window offers to choose from.
///
/// A face is a file here, not a family: `FiraCodeNerdFontMono-Regular` and
/// `FiraCodeNerdFontMono-Bold` are two of them, because they are two files and
/// either can be asked for.
pub fn installed_names() -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names: Vec<String> = installed()
            .iter()
            .filter_map(|path| path.file_name().map(|n| stem_of(&n.to_string_lossy())))
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        names.dedup();
        names
    })
}

pub fn load(preferred: &[&str]) -> Option<(String, Box<dyn GlyphSource>)> {
    let path = preferred.iter().find_map(|want| file_called(want))?;
    read(&path)
}

/// The face `name` asks for: a file, if it is a path to one, and otherwise the
/// face of that name among the ones installed — with or without the extension.
/// `None` when there is no such face, or it cannot be read, so the caller can
/// fall back to the list it would have used.
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
    let source = TrueTypeSource::from_vec(&name, bytes).ok()?;
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
        } else if path.extension().is_some_and(|e| {
            // TrueType outlines and OpenType's CFF ones, both of which the
            // glyph source reads. Not `.ttc`: a collection is several faces
            // behind one file name, and a name that cannot say which face it
            // means is a name the settings cannot keep.
            e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf")
        }) {
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

    /// Fira Code's Nerd Font build is what the text is drawn in wherever
    /// there is one, and the proportional build of it is never among the
    /// faces squint reaches for by itself.
    #[test]
    fn fira_code_is_first_and_the_proportional_build_is_not_there() {
        assert!(MONO[0].starts_with("FiraCodeNerdFontMono"), "{:?}", MONO[0]);
        assert!(!MONO.iter().any(|name| name.contains("Propo")));
        if installed_names()
            .iter()
            .any(|name| name == "FiraCodeNerdFontMono-Regular")
        {
            assert_eq!(
                choices(MONO).first().map(String::as_str),
                Some("FiraCodeNerdFontMono-Regular"),
                "installed here, so it is what the text is drawn in"
            );
        }
    }

    /// Every face offered is one that can then be loaded.
    #[test]
    fn every_choice_can_be_loaded() {
        for name in choices(MONO) {
            assert!(load_named(&name).is_some(), "{name}");
        }
    }

    /// The faces offered are in order, each once, and each one loadable by the
    /// name it is offered under. Machines without fonts have none to check.
    #[test]
    fn every_installed_face_is_listed_once_and_in_order() {
        let names = installed_names().to_vec();
        let mut sorted = names.clone();
        sorted.sort_by_key(|name| name.to_lowercase());
        assert_eq!(names, sorted, "in order");
        let mut once = names.clone();
        once.dedup();
        assert_eq!(names, once, "each once");
        for name in names.iter().take(20) {
            assert!(file_called(name).is_some(), "{name} is a face squint has");
        }
    }
}
