//! Which application the system opens a kind of file with, and making it
//! squint.
//!
//! Each system decides this its own way, and allows it to different degrees:
//!
//! - **macOS** keeps a handler per content type in LaunchServices, which an
//!   application may set for itself — if it is an application bundle
//!   LaunchServices knows, which a bare `cargo run` binary is not.
//! - **Linux** desktops read `mimeapps.list`, which `xdg-mime default` writes,
//!   naming the desktop entry a package installed. An AppImage has none.
//! - **Windows** lets only the user choose, since Windows 8: an application
//!   that writes the choice itself is ignored. squint can take the user to
//!   where the choice is made, which is all there is.
//!
//! The Settings window's File Types section and Tools ▸ Always Open with
//! squint are both built on this.

use std::path::Path;

/// A kind of file squint offers to be the default for, as each system names
/// it.
pub struct FileKind {
    pub name: &'static str,
    /// Uniform type identifiers, on macOS.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub types: &'static [&'static str],
    /// MIME types, on Linux.
    #[cfg_attr(not(any(target_os = "linux", target_os = "freebsd")), allow(dead_code))]
    pub mimes: &'static [&'static str],
    /// A few of its file name extensions, to say what it is.
    pub extensions: &'static str,
}

/// The kinds, in the order the settings list them. Every type here is also
/// declared in the app bundle, the desktop entry and the installer.
pub const KINDS: [FileKind; 7] = [
    FileKind {
        name: "Plain text",
        types: &["public.plain-text", "public.utf8-plain-text"],
        mimes: &["text/plain"],
        extensions: ".txt",
    },
    FileKind {
        name: "Logs",
        // A `.log` file is `com.apple.log`, which Console claims, not the
        // `public.log` it conforms to.
        types: &["public.log", "com.apple.log"],
        mimes: &["text/x-log"],
        extensions: ".log",
    },
    FileKind {
        name: "JSON",
        types: &["public.json", "public.ndjson"],
        mimes: &["application/json", "application/x-ndjson"],
        extensions: ".json .jsonl",
    },
    FileKind {
        name: "XML",
        types: &["public.xml"],
        mimes: &["application/xml", "text/xml"],
        extensions: ".xml",
    },
    FileKind {
        name: "YAML",
        types: &["public.yaml"],
        mimes: &["application/yaml", "application/x-yaml"],
        extensions: ".yaml .yml",
    },
    FileKind {
        name: "CSV and TSV",
        types: &[
            "public.comma-separated-values-text",
            "public.tab-separated-values-text",
        ],
        mimes: &["text/csv", "text/tab-separated-values"],
        extensions: ".csv .tsv",
    },
    FileKind {
        name: "Markdown",
        types: &["net.daringfireball.markdown"],
        mimes: &["text/markdown"],
        extensions: ".md",
    },
];

/// What this squint can do about defaults, here.
#[derive(Clone, Debug, PartialEq, Eq)]
// Each system builds only the answers it gives.
#[allow(dead_code)]
pub enum Support {
    /// It can make itself the default, and say what the default is.
    Direct,
    /// Only the user can choose, in the system's settings, which squint can
    /// open.
    SystemSettings,
    /// Not from this copy of squint, for the reason given.
    Unavailable(String),
}

/// Who opens a kind of file now.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(windows, allow(dead_code))]
pub enum Handler {
    Squint,
    /// Another application, by the name it goes by.
    Other(String),
    /// Nothing is chosen, or it cannot be read.
    Unknown,
}

/// What this squint can do about defaults, here.
pub fn support() -> Support {
    imp::support()
}

/// Who opens files of `kind` now: squint only if it opens every type the kind
/// has.
pub fn handler(kind: &FileKind) -> Handler {
    imp::handler(kind)
}

/// Makes squint the application that opens files of `kind`. What to tell the
/// user, either way.
pub fn make_default(kind: &FileKind) -> Result<String, String> {
    imp::make_default(kind)
}

/// Makes squint the application that opens files like `path`, or, where only
/// the user may choose, asks them. What to tell the user, either way.
pub fn make_default_for(path: &Path) -> Result<String, String> {
    imp::make_default_for(path)
}

/// Opens the place in the system's settings where default applications are
/// chosen.
pub fn open_system_settings() -> Result<String, String> {
    imp::open_system_settings()
}

/// The extension a message names a file by: `.log`, or its name when it has
/// none.
#[cfg_attr(windows, allow(dead_code))]
fn described(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!(".{ext} files"),
        None => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "this file".into()),
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{FileKind, Handler, Support, described};
    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSBundle, NSError, NSString, NSURL};
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    /// How long to wait for macOS to say a default is changed.
    const WAIT: Duration = Duration::from_secs(5);

    // For the `UTType` class, which is looked up by name.
    #[link(name = "UniformTypeIdentifiers", kind = "framework")]
    unsafe extern "C" {}

    /// This squint's bundle identifier, when it runs from an app bundle.
    fn bundle_id() -> Option<Retained<NSString>> {
        let bundle = NSBundle::mainBundle();
        // A binary outside a bundle can still report an identifier; only a
        // real bundle has a `.app` path.
        if !bundle.bundlePath().to_string().ends_with(".app") {
            return None;
        }
        bundle.bundleIdentifier()
    }

    pub fn support() -> Support {
        match bundle_id() {
            Some(_) => Support::Direct,
            None => Support::Unavailable(
                "Only squint.app can be the default: this squint is not running from one.".into(),
            ),
        }
    }

    /// The system's type for an identifier, or nil for one it does not know.
    fn uttype(identifier: &str) -> Option<Retained<AnyObject>> {
        let class = AnyClass::get(c"UTType")?;
        let identifier = NSString::from_str(identifier);
        // SAFETY: takes a string, returns a type or nil.
        unsafe { msg_send![class, typeWithIdentifier: &*identifier] }
    }

    /// The system's type for files with this extension, which is one it makes
    /// up when nothing declares the extension.
    fn uttype_of_extension(ext: &str) -> Option<Retained<AnyObject>> {
        let class = AnyClass::get(c"UTType")?;
        let ext = NSString::from_str(ext);
        // SAFETY: takes a string, returns a type or nil.
        unsafe { msg_send![class, typeWithFilenameExtension: &*ext] }
    }

    /// The bundle identifier of the application a double-click on a file of
    /// this type opens.
    fn handler_of(uttype: &AnyObject) -> Option<Retained<NSString>> {
        // SAFETY: takes a type, returns an application's URL or nil.
        let url: Option<Retained<NSURL>> = unsafe {
            msg_send![
                &*NSWorkspace::sharedWorkspace(),
                URLForApplicationToOpenContentType: uttype
            ]
        };
        let url = url?;
        NSBundle::bundleWithURL(&url)?.bundleIdentifier()
    }

    /// The name an application goes by, from its bundle identifier.
    fn app_name(id: &NSString) -> String {
        NSWorkspace::sharedWorkspace()
            .URLForApplicationWithBundleIdentifier(id)
            .and_then(|url| url.path())
            .and_then(|path| {
                Path::new(&path.to_string())
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            })
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| id.to_string())
    }

    /// The types a kind's files have: the ones it names, and the ones macOS
    /// gives its extensions, which can be more particular than those — and
    /// are what a double-click goes by. A type macOS makes up for an extension
    /// nothing declares is left out: squint cannot be the default for it.
    fn types_of(kind: &FileKind) -> Vec<Retained<AnyObject>> {
        let mut identifiers: Vec<String> = kind.types.iter().map(|t| t.to_string()).collect();
        let mut types: Vec<Retained<AnyObject>> =
            kind.types.iter().filter_map(|t| uttype(t)).collect();
        for ext in kind.extensions.split_whitespace() {
            let Some(t) = uttype_of_extension(ext.trim_start_matches('.')) else {
                continue;
            };
            let identifier = identifier_of(&t);
            if !identifier.starts_with("dyn.") && !identifiers.contains(&identifier) {
                identifiers.push(identifier);
                types.push(t);
            }
        }
        types
    }

    fn identifier_of(uttype: &AnyObject) -> String {
        // SAFETY: `-[UTType identifier]` returns a string.
        let identifier: Retained<NSString> = unsafe { msg_send![uttype, identifier] };
        identifier.to_string()
    }

    pub fn handler(kind: &FileKind) -> Handler {
        let ours = bundle_id().map(|id| id.to_string().to_lowercase());
        let mut other = None;
        for uttype in types_of(kind) {
            match handler_of(&uttype) {
                Some(id) if Some(id.to_string().to_lowercase()) == ours => {}
                Some(id) => {
                    other.get_or_insert_with(|| app_name(&id));
                }
                None => return Handler::Unknown,
            }
        }
        match other {
            Some(name) => Handler::Other(name),
            None => Handler::Squint,
        }
    }

    /// Makes the application `id` the one that opens files of `uttype`, and
    /// waits for macOS to say it has.
    ///
    /// This is NSWorkspace's `setDefaultApplicationAtURL:toOpenContentType:
    /// completionHandler:`. The older `LSSetDefaultRoleHandlerForContentType`
    /// still answers yes, and as often as not changes nothing.
    fn set(uttype: &AnyObject, id: &NSString) -> Result<(), String> {
        let workspace = NSWorkspace::sharedWorkspace();
        let app = workspace
            .URLForApplicationWithBundleIdentifier(id)
            .ok_or_else(|| format!("there is no application {id}"))?;
        let (tx, rx) = mpsc::channel();
        let done = RcBlock::new(move |error: *mut NSError| {
            // SAFETY: nil, or an error that lives for the call.
            let why = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
            let _ = tx.send(why);
        });
        // SAFETY: an application's URL, a type, and a block that takes an
        // error, which macOS calls once, on a queue of its own.
        let () = unsafe {
            msg_send![
                &*workspace,
                setDefaultApplicationAtURL: &*app,
                toOpenContentType: uttype,
                completionHandler: &*done
            ]
        };
        match rx.recv_timeout(WAIT) {
            Ok(None) => Ok(()),
            Ok(Some(why)) => Err(why),
            Err(_) => Err("macOS did not answer".into()),
        }
    }

    pub fn make_default(kind: &FileKind) -> Result<String, String> {
        let Some(id) = bundle_id() else {
            return Err("only squint.app can be the default".into());
        };
        for uttype in types_of(kind) {
            set(&uttype, &id)
                .map_err(|why| format!("macOS would not make squint open {}: {why}", kind.name))?;
        }
        Ok(format!("squint now opens {}", kind.name))
    }

    pub fn make_default_for(path: &Path) -> Result<String, String> {
        let Some(id) = bundle_id() else {
            return Err("only squint.app can be the default".into());
        };
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return Err("a file with no extension has no kind to choose squint for".into());
        };
        let uttype = uttype_of_extension(ext)
            .ok_or_else(|| format!("macOS has no kind of file for .{ext}"))?;
        set(&uttype, &id)
            .map_err(|why| format!("macOS would not make squint open .{ext} files: {why}"))?;
        Ok(format!("squint now opens {}", described(path)))
    }

    pub fn open_system_settings() -> Result<String, String> {
        Err("macOS chooses defaults in Finder's Get Info, not in its settings".into())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Changes what macOS keeps, so it is run by hand. It sets the
        /// application for a type few files have, reads it back, and puts the
        /// one there was back.
        #[test]
        #[ignore = "changes a default application"]
        fn a_default_set_is_the_default_read() {
            let uttype = uttype("public.xbitmap-image").expect("a type");
            let before = handler_of(&uttype).expect("an application to put back");
            for app in [
                "com.apple.TextEdit",
                "com.apple.Preview",
                "com.apple.TextEdit",
            ] {
                set(&uttype, &NSString::from_str(app)).expect("set");
                let now = handler_of(&uttype).expect("an application").to_string();
                assert!(now.eq_ignore_ascii_case(app), "{app} set, {now} read");
            }
            set(&uttype, &before).expect("put back");
            assert_eq!(
                app_name(&NSString::from_str("com.apple.TextEdit")),
                "TextEdit"
            );
        }

        /// What a double-click goes by is in what Make Default changes.
        #[test]
        fn a_kind_covers_the_types_its_extensions_have() {
            let logs = super::super::KINDS
                .iter()
                .find(|k| k.name == "Logs")
                .unwrap();
            let names: Vec<String> = types_of(logs).iter().map(|t| identifier_of(t)).collect();
            assert!(names.contains(&"com.apple.log".to_string()), "{names:?}");
            for kind in &super::super::KINDS {
                assert!(
                    types_of(kind)
                        .iter()
                        .all(|t| !identifier_of(t).starts_with("dyn.")),
                    "{}",
                    kind.name
                );
            }
        }

        #[test]
        fn a_made_up_extension_still_has_a_type() {
            assert!(uttype_of_extension("squint-selftest").is_some());
            assert!(uttype("public.json").is_some());
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod imp {
    use super::{FileKind, Handler, Support, described};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// The desktop entry the packages install.
    const ENTRY: &str = "squint.desktop";

    /// Where desktop entries are looked for, the user's own first.
    fn entry_installed() -> bool {
        let mut dirs: Vec<PathBuf> = dirs::data_dir().into_iter().collect();
        let system = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        dirs.extend(system.split(':').map(PathBuf::from));
        dirs.iter()
            .any(|dir| dir.join("applications").join(ENTRY).is_file())
    }

    fn xdg_mime(args: &[&str]) -> Option<String> {
        let out = Command::new("xdg-mime").args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn support() -> Support {
        if std::env::var_os("APPIMAGE").is_some() {
            return Support::Unavailable(
                "An AppImage installs no desktop entry for the default to name. Install the .deb, .rpm or Arch package for that.".into(),
            );
        }
        if !entry_installed() {
            return Support::Unavailable(
                "squint's desktop entry, squint.desktop, is not installed.".into(),
            );
        }
        if xdg_mime(&["--version"]).is_none() {
            return Support::Unavailable("xdg-mime, from xdg-utils, is not installed.".into());
        }
        Support::Direct
    }

    pub fn handler(kind: &FileKind) -> Handler {
        let mut other = None;
        for mime in kind.mimes {
            match xdg_mime(&["query", "default", mime]).as_deref() {
                Some(ENTRY) => {}
                Some("") | None => return Handler::Unknown,
                Some(entry) => {
                    other.get_or_insert_with(|| entry.trim_end_matches(".desktop").to_string());
                }
            }
        }
        match other {
            Some(name) => Handler::Other(name),
            None => Handler::Squint,
        }
    }

    pub fn make_default(kind: &FileKind) -> Result<String, String> {
        let mut args = vec!["default", ENTRY];
        args.extend(kind.mimes);
        xdg_mime(&args)
            .map(|_| format!("squint now opens {}", kind.name))
            .ok_or_else(|| format!("xdg-mime would not make squint open {}", kind.name))
    }

    pub fn make_default_for(path: &Path) -> Result<String, String> {
        let file = path.to_string_lossy();
        let mime = xdg_mime(&["query", "filetype", &file])
            .filter(|m| !m.is_empty())
            .ok_or_else(|| format!("xdg-mime cannot tell what kind of file {file} is"))?;
        xdg_mime(&["default", ENTRY, &mime])
            .map(|_| format!("squint now opens {} ({mime})", described(path)))
            .ok_or_else(|| format!("xdg-mime would not make squint open {mime}"))
    }

    pub fn open_system_settings() -> Result<String, String> {
        Err("the desktop's own settings choose defaults".into())
    }
}

#[cfg(windows)]
mod imp {
    use super::{FileKind, Handler, Support};
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::Command;

    /// Keeps a console window from flashing up for the helper.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn support() -> Support {
        Support::SystemSettings
    }

    pub fn handler(_kind: &FileKind) -> Handler {
        // The user's choice is kept where Windows guards it, and reading it
        // would say little the Default apps page does not show better.
        Handler::Unknown
    }

    pub fn make_default(_kind: &FileKind) -> Result<String, String> {
        open_system_settings()
    }

    pub fn make_default_for(path: &Path) -> Result<String, String> {
        // Windows' own "How do you want to open this file?", with its Always
        // box: the one place a program may put the question.
        Command::new("rundll32.exe")
            .arg("shell32.dll,OpenAs_RunDLL")
            .arg(path)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| "choose squint, and tick Always".into())
            .map_err(|e| format!("could not ask Windows: {e}"))
    }

    pub fn open_system_settings() -> Result<String, String> {
        // The Default apps page for squint itself, as the installer
        // registered it.
        Command::new("cmd")
            .args([
                "/C",
                "start",
                "",
                "ms-settings:defaultapps?registeredAppUser=squint",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(|_| "choose squint for the file types in Default apps".into())
            .map_err(|e| format!("could not open Default apps: {e}"))
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd",
    windows
)))]
mod imp {
    use super::{FileKind, Handler, Support};
    use std::path::Path;

    pub fn support() -> Support {
        Support::Unavailable("squint does not know how this system chooses defaults.".into())
    }

    pub fn handler(_kind: &FileKind) -> Handler {
        Handler::Unknown
    }

    pub fn make_default(_kind: &FileKind) -> Result<String, String> {
        Err("not on this system".into())
    }

    pub fn make_default_for(_path: &Path) -> Result<String, String> {
        Err("not on this system".into())
    }

    pub fn open_system_settings() -> Result<String, String> {
        Err("not on this system".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_names_its_types_for_each_system() {
        for kind in &KINDS {
            assert!(!kind.types.is_empty(), "{}", kind.name);
            assert!(!kind.mimes.is_empty(), "{}", kind.name);
            assert!(kind.extensions.starts_with('.'), "{}", kind.name);
        }
    }

    #[test]
    fn the_app_bundle_declares_every_type_offered() {
        let plist = include_str!("../../packaging/macos/Info.plist");
        for kind in &KINDS {
            for t in kind.types {
                assert!(plist.contains(&format!("<string>{t}</string>")), "{t}");
            }
        }
    }

    #[test]
    fn the_desktop_entry_declares_every_type_offered() {
        let entry = include_str!("../../packaging/linux/squint.desktop");
        let mimes = entry
            .lines()
            .find_map(|l| l.strip_prefix("MimeType="))
            .unwrap();
        for kind in &KINDS {
            for m in kind.mimes {
                assert!(mimes.split(';').any(|x| x == *m), "{m}");
            }
        }
    }

    #[test]
    fn a_message_names_a_file_by_its_extension() {
        assert_eq!(described(Path::new("/var/log/system.log")), ".log files");
        assert_eq!(described(Path::new("/etc/hosts")), "hosts");
    }
}
