//! EditorConfig: the settings a project gives every editor that opens it.
//!
//! As <https://editorconfig.org> specifies: `.editorconfig` files are read
//! from the file's directory upwards until one says `root = true`, and every
//! section whose glob matches the file's path applies, later sections and
//! nearer files winning. A glob without a `/` matches the file's name in any
//! directory below; one with a `/` is anchored at the directory its
//! `.editorconfig` is in.
//!
//! Only the lookup and the globs live here. What a property means is up to
//! whoever asks — for squint, the formatters (`format::Style`).

use std::fs;
use std::path::{Path, PathBuf};

/// The properties that apply to one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Properties {
    /// Lower-cased keys and their values as written, one entry per key.
    values: Vec<(String, String)>,
    /// The `.editorconfig` files with a section for the file, nearest first.
    sources: Vec<PathBuf>,
}

impl Properties {
    /// A property's value, or `None` if it is not set or set to `unset`.
    /// Keys are matched without regard to case; values come as written.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.eq_ignore_ascii_case("unset"))
    }

    /// The `.editorconfig` files that had a section for the file, nearest
    /// first. Empty when nothing applied.
    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// How many columns a tab is, as the spec works it out: `tab_width`, or
    /// failing that a numeric `indent_size`. `None` when neither says.
    pub fn tab_width(&self) -> Option<u8> {
        let width = |key| {
            self.get(key)
                .and_then(|v| v.parse::<u8>().ok())
                .filter(|&n| n > 0)
        };
        width("tab_width").or_else(|| width("indent_size"))
    }

    fn set(&mut self, key: &str, value: &str) {
        let key = key.to_ascii_lowercase();
        match self.values.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value.to_string(),
            None => self.values.push((key, value.to_string())),
        }
    }
}

impl<K: AsRef<str>, V: AsRef<str>> FromIterator<(K, V)> for Properties {
    /// Properties set in order, as if from one section.
    fn from_iter<I: IntoIterator<Item = (K, V)>>(pairs: I) -> Self {
        let mut props = Properties::default();
        for (k, v) in pairs {
            props.set(k.as_ref(), v.as_ref());
        }
        props
    }
}

/// The properties EditorConfig gives the file at `path`, which need not
/// exist yet. A `.editorconfig` that cannot be read is passed over: a
/// missing or broken config must never stop the work it only tunes.
pub fn properties_for(path: &Path) -> Properties {
    let Ok(path) = std::path::absolute(path) else {
        return Properties::default();
    };
    let mut files = Vec::new();
    let mut dir = path.parent();
    while let Some(d) = dir {
        let file = d.join(".editorconfig");
        if let Ok(text) = fs::read_to_string(&file) {
            let parsed = parse(&text);
            let root = parsed.root;
            files.push((d.to_path_buf(), file, parsed));
            if root {
                break;
            }
        }
        dir = d.parent();
    }

    let mut props = Properties::default();
    // Farthest first, so nearer files overwrite what it set.
    for (dir, file, parsed) in files.iter().rev() {
        let Some(rel) = relative(&path, dir) else {
            continue;
        };
        let mut applied = false;
        for section in &parsed.sections {
            if section_matches(&section.glob, &rel) {
                applied = true;
                for (key, value) in &section.props {
                    props.set(key, value);
                }
            }
        }
        if applied {
            props.sources.insert(0, file.clone());
        }
    }
    props
}

/// `path` below `dir`, with `/` between its components whatever the platform.
fn relative(path: &Path, dir: &Path) -> Option<String> {
    let rel = path.strip_prefix(dir).ok()?;
    let parts: Vec<_> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    Some(parts.join("/"))
}

#[derive(Debug, Default)]
struct ConfigFile {
    root: bool,
    sections: Vec<Section>,
}

#[derive(Debug)]
struct Section {
    glob: String,
    props: Vec<(String, String)>,
}

fn parse(text: &str) -> ConfigFile {
    let mut file = ConfigFile::default();
    for raw in text.trim_start_matches('\u{feff}').lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.len() >= 2 && line.starts_with('[') && line.ends_with(']') {
            file.sections.push(Section {
                glob: line[1..line.len() - 1].to_string(),
                props: Vec::new(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match file.sections.last_mut() {
            Some(section) => section.props.push((key, value.to_string())),
            // Before the first section only `root` means anything.
            None if key == "root" => file.root = value.eq_ignore_ascii_case("true"),
            None => {}
        }
    }
    file
}

// ---- globs ------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Lit(char),
    /// `*`: any run of characters but `/`.
    AnyName,
    /// `**`: any run of characters, `/` included.
    AnyPath,
    /// `?`: one character but `/`.
    AnyChar,
    /// `[...]` or `[!...]`, as inclusive ranges.
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
    /// `{a,b,...}`.
    Alt(Vec<Vec<Token>>),
    /// `{n..m}`: an integer from `n` to `m`.
    Numbers(i64, i64),
}

/// Whether a section's glob covers `rel`: a path relative to the directory
/// of the `.editorconfig` the section is in.
fn section_matches(glob: &str, rel: &str) -> bool {
    let pattern = if glob.contains('/') {
        glob.strip_prefix('/').unwrap_or(glob).to_string()
    } else {
        format!("**/{glob}")
    };
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = rel.chars().collect();
    matches(&compile(&pattern), &text)
}

fn compile(p: &[char]) -> Vec<Token> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            '\\' if i + 1 < p.len() => {
                out.push(Token::Lit(p[i + 1]));
                i += 2;
            }
            '*' if p.get(i + 1) == Some(&'*') => {
                out.push(Token::AnyPath);
                i += 2;
            }
            '*' => {
                out.push(Token::AnyName);
                i += 1;
            }
            '?' => {
                out.push(Token::AnyChar);
                i += 1;
            }
            '[' => match class(p, i) {
                Some((token, next)) => {
                    out.push(token);
                    i = next;
                }
                None => {
                    out.push(Token::Lit('['));
                    i += 1;
                }
            },
            '{' => match brace(p, i) {
                Some((tokens, next)) => {
                    out.extend(tokens);
                    i = next;
                }
                None => {
                    out.push(Token::Lit('{'));
                    i += 1;
                }
            },
            c => {
                out.push(Token::Lit(c));
                i += 1;
            }
        }
    }
    out
}

/// The class opening at `p[at]`, and where the pattern resumes after it.
/// `None` when it is not a class, and the `[` is literal.
fn class(p: &[char], at: usize) -> Option<(Token, usize)> {
    let mut i = at + 1;
    let negated = p.get(i) == Some(&'!');
    if negated {
        i += 1;
    }
    let mut ranges = Vec::new();
    while i < p.len() && p[i] != ']' {
        let c = if p[i] == '\\' && i + 1 < p.len() {
            i += 1;
            p[i]
        } else {
            p[i]
        };
        if c == '/' {
            return None;
        }
        if p.get(i + 1) == Some(&'-') && p.get(i + 2).is_some_and(|&e| e != ']') {
            ranges.push((c, p[i + 2]));
            i += 3;
        } else {
            ranges.push((c, c));
            i += 1;
        }
    }
    (i < p.len() && !ranges.is_empty()).then_some((Token::Class { negated, ranges }, i + 1))
}

/// The braces opening at `p[at]`: alternatives, a number range, or — with
/// neither a comma nor a range inside — the literal text. `None` when they
/// are never closed.
fn brace(p: &[char], at: usize) -> Option<(Vec<Token>, usize)> {
    let mut depth = 0;
    let mut commas = Vec::new();
    let mut i = at;
    let close = loop {
        match p.get(i)? {
            '\\' => i += 1,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    break i;
                }
            }
            ',' if depth == 1 => commas.push(i),
            _ => {}
        }
        i += 1;
    };
    if !commas.is_empty() {
        let mut alts = Vec::new();
        let mut start = at + 1;
        for &comma in commas.iter().chain(std::iter::once(&close)) {
            alts.push(compile(&p[start..comma]));
            start = comma + 1;
        }
        return Some((vec![Token::Alt(alts)], close + 1));
    }
    let inner: String = p[at + 1..close].iter().collect();
    if let Some((lo, hi)) = inner.split_once("..")
        && let (Ok(lo), Ok(hi)) = (lo.parse::<i64>(), hi.parse::<i64>())
    {
        return Some((vec![Token::Numbers(lo.min(hi), lo.max(hi))], close + 1));
    }
    let literal = p[at..=close].iter().map(|&c| Token::Lit(c)).collect();
    Some((literal, close + 1))
}

fn matches(tokens: &[Token], text: &[char]) -> bool {
    let Some((token, rest)) = tokens.split_first() else {
        return text.is_empty();
    };
    match token {
        Token::Lit(c) => text.first() == Some(c) && matches(rest, &text[1..]),
        Token::AnyChar => text.first().is_some_and(|&c| c != '/') && matches(rest, &text[1..]),
        Token::Class { negated, ranges } => {
            text.first().is_some_and(|&c| {
                c != '/' && ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi) != *negated
            }) && matches(rest, &text[1..])
        }
        Token::AnyName => {
            for i in 0..=text.len() {
                if matches(rest, &text[i..]) {
                    return true;
                }
                if i < text.len() && text[i] == '/' {
                    break;
                }
            }
            false
        }
        Token::AnyPath => {
            // `**/` also stands for no directories at all, so `**/x`
            // matches `x` itself.
            if let Some((Token::Lit('/'), after)) = rest.split_first()
                && matches(after, text)
            {
                return true;
            }
            (0..=text.len()).any(|i| matches(rest, &text[i..]))
        }
        Token::Alt(alts) => alts.iter().any(|alt| {
            let mut seq = alt.clone();
            seq.extend_from_slice(rest);
            matches(&seq, text)
        }),
        Token::Numbers(lo, hi) => {
            let sign = usize::from(text.first() == Some(&'-'));
            let digits = text[sign..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .count();
            (sign + 1..=sign + digits).rev().any(|end| {
                let n: String = text[..end].iter().collect();
                n.parse::<i64>()
                    .is_ok_and(|n| *lo <= n && n <= *hi && matches(rest, &text[end..]))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn check(glob: &str, path: &str, expected: bool) {
        assert_eq!(section_matches(glob, path), expected, "[{glob}] on {path}");
    }

    #[test]
    fn a_glob_without_a_slash_matches_a_name_anywhere() {
        check("*", "a.json", true);
        check("*", "sub/a.json", true);
        check("*.json", "deep/er/x.json", true);
        check("*.json", "x.jsonl", false);
        check("?.json", "a.json", true);
        check("?.json", "ab.json", false);
        check("Makefile", "src/Makefile", true);
    }

    #[test]
    fn a_glob_with_a_slash_is_anchored_where_its_file_is() {
        check("/top.json", "top.json", true);
        check("/top.json", "sub/top.json", false);
        check("sub/*.json", "sub/x.json", true);
        check("sub/*.json", "other/sub/x.json", false);
        check("sub/*.json", "sub/deeper/x.json", false);
        check("lib/**.js", "lib/a/b/c.js", true);
        check("**/vendor/*", "vendor/x", true);
        check("**/vendor/*", "a/b/vendor/x", true);
    }

    #[test]
    fn classes_braces_and_ranges() {
        check("[abc].txt", "b.txt", true);
        check("[!abc].txt", "b.txt", false);
        check("[a-c].txt", "c.txt", true);
        check("[!a-c].txt", "d.txt", true);
        check("*.{json,xml}", "a.xml", true);
        check("*.{json,xml}", "a.txt", false);
        check("{src,lib}/*.json", "lib/a.json", true);
        check("{src,lib}/*.json", "lib/x/a.json", false);
        check("*.{js,{c,h}pp}", "x.hpp", true);
        check("file{1..3}.txt", "file2.txt", true);
        check("file{1..3}.txt", "file4.txt", false);
        check("file{1..3}.txt", "file12.txt", false);
        check("v{-1..1}", "v-1", true);
    }

    #[test]
    fn what_is_not_special_is_literal() {
        check("\\*.json", "*.json", true);
        check("\\*.json", "a.json", false);
        check("{single}.json", "{single}.json", true);
        check("{single}.json", "single.json", false);
        check("a[", "a[", true);
        check("a{b", "a{b", true);
    }

    #[test]
    fn parses_sections_properties_root_and_comments() {
        let file = parse(
            "\u{feff}# top\nroot = TRUE\n\n[*.json]\n; note\nIndent_Style = Tab\nindent_size=4\n[[ab].txt]\nx = y\n",
        );
        assert!(file.root);
        assert_eq!(file.sections.len(), 2);
        assert_eq!(file.sections[0].glob, "*.json");
        assert_eq!(
            file.sections[0].props,
            [
                ("indent_style".to_string(), "Tab".to_string()),
                ("indent_size".to_string(), "4".to_string())
            ]
        );
        assert_eq!(file.sections[1].glob, "[ab].txt");
    }

    #[test]
    fn the_tab_width_falls_back_to_a_numeric_indent_size() {
        let props = |pairs: &[(&str, &str)]| pairs.iter().copied().collect::<Properties>();
        assert_eq!(
            props(&[("tab_width", "8"), ("indent_size", "2")]).tab_width(),
            Some(8)
        );
        assert_eq!(props(&[("indent_size", "3")]).tab_width(), Some(3));
        assert_eq!(props(&[("indent_size", "tab")]).tab_width(), None);
        assert_eq!(
            props(&[("tab_width", "unset"), ("indent_size", "5")]).tab_width(),
            Some(5)
        );
        assert_eq!(props(&[("tab_width", "0")]).tab_width(), None);
        assert_eq!(props(&[]).tab_width(), None);
    }

    #[test]
    fn nearer_files_and_later_sections_win_and_root_stops_the_search() {
        let dir = tempfile::tempdir().unwrap();
        let above = dir.path();
        fs::write(
            above.join(".editorconfig"),
            "[*]\nindent_size = 3\nend_of_line = cr\n",
        )
        .unwrap();
        let proj = above.join("proj");
        fs::create_dir(&proj).unwrap();
        fs::write(
            proj.join(".editorconfig"),
            "root = true\n\n[*]\nindent_style = space\nindent_size = 4\n\n[*.xml]\nindent_style = tab\n",
        )
        .unwrap();
        let sub = proj.join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(
            sub.join(".editorconfig"),
            "[*.json]\nindent_size = 8\ninsert_final_newline = false\n\n[data.json]\ninsert_final_newline = unset\n",
        )
        .unwrap();

        let json = properties_for(&sub.join("data.json"));
        assert_eq!(json.get("indent_style"), Some("space"));
        assert_eq!(json.get("INDENT_SIZE"), Some("8"), "the nearer file wins");
        assert_eq!(
            json.get("insert_final_newline"),
            None,
            "`unset` takes it back"
        );
        assert_eq!(
            json.get("end_of_line"),
            None,
            "the root file stops the search before the one above it"
        );
        assert_eq!(
            json.sources(),
            [sub.join(".editorconfig"), proj.join(".editorconfig")]
        );

        let xml = properties_for(&sub.join("feed.xml"));
        assert_eq!(
            xml.get("indent_style"),
            Some("tab"),
            "the later section wins"
        );
        assert_eq!(xml.get("indent_size"), Some("4"));
        assert_eq!(xml.sources(), [proj.join(".editorconfig")]);
    }
}
