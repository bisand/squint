//! Finds a monospace face for the text and a UI face for the chrome on the
//! machine we run on, falling back to Denise's built-in bitmap font. Lifted
//! from ctail, and since the settings window, able to load a face the settings
//! name — one of the faces squint looks for, one of the faces installed, or
//! any font file at all — and to list every face there is, for the settings to
//! choose from.

use denise_text::{GlyphSource, TrueTypeSource};
use std::collections::HashMap;
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

/// The size a face is asked for a glyph at to find out whether it draws
/// anything. Any size would do; this one is near what the chrome uses.
const INK: u16 = 13;

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

/// The first of `preferred` this machine has and can draw in.
///
/// Walks on past a face it cannot read, and past one it can read and not draw
/// in, rather than stopping at the first file of the right name: a face that
/// parses and has no ink in it would otherwise leave the window with nothing
/// written anywhere in it.
pub fn load(preferred: &[&str]) -> Option<(String, Box<dyn GlyphSource>)> {
    preferred.iter().find_map(|want| read(&file_called(want)?))
}

/// The face `name` asks for: a file, if it is a path to one, and otherwise the
/// face of that name among the ones installed — with or without the extension.
/// `None` when there is no such face, or it cannot be read, or nothing can be
/// drawn in it, so the caller can fall back to the list it would have used —
/// and say that it did.
pub fn load_named(name: &str) -> Option<(String, Box<dyn GlyphSource>)> {
    let as_file = Path::new(name);
    if as_file.is_file() {
        return read(as_file);
    }
    read(&file_called(name)?)
}

/// The faces of `preferred` this machine has and can draw in, by the name the
/// settings name them: the file's name without `.ttf`.
pub fn choices(preferred: &[&str]) -> Vec<String> {
    preferred
        .iter()
        .filter(|want| file_called(want).is_some_and(|path| drawable(&path)))
        .map(|want| stem_of(want))
        .collect()
}

/// Whether anything can be drawn in the face at `path`.
///
/// Finding out costs reading the file, and the settings form asks again every
/// time one of its rows changes, so the faces squint looks for by itself are
/// settled once, between them, the first time any of them is asked about. A
/// face outside those lists is read where it is asked for, so this is right
/// for any list and quick for the ones the settings actually offer.
///
/// Opening a window does not ask — [`load`] reads the faces it needs and no
/// others — so a squint that never opens the settings never pays for this.
fn drawable(path: &Path) -> bool {
    static KNOWN: OnceLock<HashMap<PathBuf, bool>> = OnceLock::new();
    let known = KNOWN.get_or_init(|| {
        MONO.iter()
            .chain(UI)
            .filter_map(|want| file_called(want))
            .map(|path| {
                let draws = read(&path).is_some();
                (path, draws)
            })
            .collect()
    });
    known
        .get(path)
        .copied()
        .unwrap_or_else(|| read(path).is_some())
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

/// The face in `path`, or `None` where it is not one to draw in: a file that
/// is not a font at all, or one that parses with no ink in it.
fn read(path: &Path) -> Option<(String, Box<dyn GlyphSource>)> {
    let name = path.display().to_string();
    let bytes = std::fs::read(path).ok()?;
    let mut source = TrueTypeSource::from_vec(&name, bytes).ok()?;
    if !draws(&mut source) {
        return None;
    }
    Some((name, Box::new(source)))
}

/// Whether there is any ink in `face`.
///
/// A face can parse, report a glyph for a character, and still rasterise to an
/// empty mask — a variable font read without variable-font support does
/// exactly that. Nothing downstream notices: the lines are the right height
/// and every one of them is blank. Asking here costs one outline, once per
/// face, and turns a window with no words in it into the next face down the
/// list.
///
/// Only a face that has one of these characters and draws none of them is
/// turned away. One that has none of them to ask about — a face of icons
/// alone — is taken at its word: it is not a face to read a file in, but that
/// is not the same as a face with nothing in it, and somebody may have chosen
/// it knowing exactly what it is.
fn draws(face: &mut TrueTypeSource) -> bool {
    let mut asked = false;
    for ch in ['n', '0', '.'] {
        if !face.contains(ch) {
            continue;
        }
        asked = true;
        let inked = face
            .glyph_id(ch)
            .and_then(|id| face.rasterise(id, INK))
            .is_some_and(|glyph| glyph.coverage.iter().any(|&ink| ink > 0));
        if inked {
            return true;
        }
    }
    !asked
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

    /// The faces squint draws in have ink in them.
    ///
    /// A face can parse, and report a glyph for `a`, and still rasterise to
    /// nothing at all: macOS's own UI face, SFNS.ttf, is a variable font, and
    /// a glyph source built without variable-font support reads its outlines
    /// as empty. Nothing says so — the window opens, the lines are the right
    /// height, and there are no words in any of them — so it is said here.
    /// See the note beside `ab_glyph` in `desktop/Cargo.toml`.
    #[test]
    fn the_faces_squint_draws_in_have_ink_in_them() {
        for preferred in [MONO, UI] {
            // A machine with none of them has nothing to check.
            let Some((name, mut face)) = load(preferred) else {
                continue;
            };
            let id = face.glyph_id('a').expect("every face here has an a");
            let glyph = face.rasterise(id, 16).expect("and can draw it");
            assert!(
                glyph.coverage.iter().any(|&ink| ink > 0),
                "{name} draws nothing"
            );
        }
    }

    /// Every face offered is one that can then be loaded and drawn in.
    #[test]
    fn every_choice_can_be_loaded() {
        for preferred in [MONO, UI] {
            for name in choices(preferred) {
                assert!(load_named(&name).is_some(), "{name}");
            }
        }
    }

    /// A face squint does not look for by itself is answered for like any
    /// other. What is kept between asks is a shortcut past reading the file,
    /// not the whole of what can be offered, and a list of other faces once
    /// came back empty for want of being in it.
    #[test]
    fn a_face_outside_the_lists_squint_looks_for_is_still_offered() {
        let looked_for = |name: &str| {
            MONO.iter()
                .chain(UI)
                .any(|want| stem_of(want).eq_ignore_ascii_case(name))
        };
        // A face this machine has, is not on either list, and can be drawn in.
        // A machine with no such face has nothing to check.
        let Some(other) = installed_names()
            .iter()
            .find(|name| !looked_for(name) && load_named(name).is_some())
        else {
            return;
        };
        assert_eq!(choices(&[other.as_str()]), vec![other.clone()], "{other}");
    }

    /// A file that is not a face is not one to draw in, and a list of faces is
    /// walked past the ones that cannot be used rather than stopped at the
    /// first of the right name.
    #[test]
    fn a_face_that_cannot_be_drawn_in_is_walked_past() {
        let dir = tempfile::tempdir().expect("a directory");
        let junk = dir.path().join("NotAFace.ttf");
        std::fs::write(&junk, b"not a font at all").expect("written");
        assert!(read(&junk).is_none(), "there is nothing to draw in it");
        assert!(load_named(junk.to_str().expect("a path")).is_none());

        // Machines without fonts have no list to walk.
        let here = choices(MONO);
        let Some(first) = here.first() else {
            return;
        };
        let names = ["NoSuchFaceIsInstalledAnywhere.ttf", first.as_str()];
        let (found, _) = load(&names).expect("the second of them");
        assert_eq!(stem_of(&found), *first, "walked past the one that is not");
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
